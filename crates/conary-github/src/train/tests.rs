use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};

use super::*;
use crate::api::{CheckRun, Commit, PullRequest, Pulls, Review, Status as CommitStatus};
use conary_core::op::Authority;

const TRUNK: &str = "1111111111111111111111111111111111111111";
const FORK: &str = "2222222222222222222222222222222222222222";

fn head(pr: u64) -> String {
    format!("{pr:0>40}")
}

fn repo() -> Repo {
    "FieldmouseWorks/remi".parse().unwrap()
}

fn config() -> Config {
    Config {
        reviewers: vec!["fieldmouseworks-steward[bot]".into()],
        own_contexts: vec!["review-settled".into()],
    }
}

fn settings() -> Settings {
    Settings {
        trunk: "main".into(),
        size: 8,
    }
}

/// GitHub, as far as the train can tell: open PRs at their heads, CI
/// results by commit, and branches that merges really move, each holding
/// the heads merged into it. A head in `conflicts` won't merge onto
/// anything that holds another head.
#[derive(Default)]
struct Hub {
    pulls: RefCell<BTreeMap<u64, String>>,
    checks: HashMap<String, Value>,
    statuses: HashMap<String, Value>,
    conflicts: HashSet<String>,
    branches: RefCell<BTreeMap<String, String>>,
    /// What each commit holds: the heads merged into it.
    holds: RefCell<HashMap<String, Vec<String>>>,
    /// Every merge made, as (branch, head).
    merges: RefCell<Vec<(String, String)>>,
    made: RefCell<u64>,
}

impl Hub {
    fn new(prs: &[u64]) -> Self {
        let hub = Self::default();
        for pr in prs {
            hub.pulls.borrow_mut().insert(*pr, head(*pr));
        }
        hub.branches
            .borrow_mut()
            .insert("main".into(), TRUNK.into());
        hub.holds.borrow_mut().insert(TRUNK.into(), Vec::new());
        hub
    }

    fn branch_at(&self, name: &str) -> Option<String> {
        self.branches.borrow().get(name).cloned()
    }

    fn names(&self) -> Vec<String> {
        self.branches.borrow().keys().cloned().collect()
    }

    /// The heads a branch holds, in the order they were merged.
    fn held(&self, name: &str) -> Vec<String> {
        let at = self.branch_at(name).unwrap();
        self.holds.borrow()[&at].clone()
    }
}

/// A tree is named by what its commit holds, so two commits holding the
/// same heads in the same order have one tree.
fn tree_of(holds: &[String]) -> String {
    let n = holds.iter().fold(7u64, |n, h| {
        n.wrapping_mul(31)
            .wrapping_add(u64::from_str_radix(&h[24..], 16).unwrap())
    });
    format!("{n:0>40x}")
}

fn parse<T: serde::de::DeserializeOwned>(v: &Value) -> T {
    serde_json::from_value(v.clone()).unwrap()
}

impl Source for Hub {
    fn pull(&self, _: &str, number: u64) -> Result<PullRequest, ApiError> {
        let head = self.pulls.borrow()[&number].clone();
        Ok(parse(&json!({
            "number": number,
            "state": "open",
            "title": "remi: a change",
            "body": "",
            "user": {"login": "TusanHomichi", "type": "User"},
            "merged_at": null,
            "merge_commit_sha": null,
            "base": {"sha": TRUNK, "ref": "main", "repo": {"default_branch": "main"}},
            "head": {"sha": head, "ref": "claude/x"},
        })))
    }

    fn pulls(&self, _: &str, _: Pulls) -> Result<Vec<PullRequest>, ApiError> {
        unreachable!("the train reads one PR at a time")
    }

    fn check_runs(&self, _: &str, sha: &str) -> Result<Vec<CheckRun>, ApiError> {
        Ok(self.checks.get(sha).map_or_else(Vec::new, parse))
    }

    fn statuses(&self, _: &str, sha: &str) -> Result<Vec<CommitStatus>, ApiError> {
        Ok(self.statuses.get(sha).map_or_else(Vec::new, parse))
    }

    fn reviews(&self, _: &str, _: u64) -> Result<Vec<Review>, ApiError> {
        Ok(Vec::new())
    }

    fn commit(&self, _: &str, _: &str) -> Result<Commit, ApiError> {
        unreachable!("no PR here has merged")
    }

    fn commit_pulls(&self, _: &str, _: &str) -> Result<Vec<u64>, ApiError> {
        Ok(Vec::new())
    }

    fn retargeted(&self, _: &str, _: u64) -> Result<Option<String>, ApiError> {
        Ok(None)
    }

    fn merge_base(&self, _: &str, _: &str, _: &str) -> Result<String, ApiError> {
        Ok(FORK.into())
    }
}

impl Forge for Hub {
    fn branch(&self, _: &str, name: &str) -> Result<Option<String>, ApiError> {
        Ok(self.branch_at(name))
    }

    fn branches(&self, _: &str, prefix: &str) -> Result<Vec<String>, ApiError> {
        Ok(self
            .names()
            .into_iter()
            .filter(|n| n.starts_with(prefix))
            .collect())
    }

    fn create_branch(&self, _: &str, name: &str, sha: &str) -> Result<(), ApiError> {
        let mut branches = self.branches.borrow_mut();
        if branches.contains_key(name) {
            return Err(ApiError::new(format!("{name}: Reference already exists")));
        }
        branches.insert(name.into(), sha.into());
        Ok(())
    }

    fn delete_branch(&self, _: &str, name: &str) -> Result<(), ApiError> {
        self.branches.borrow_mut().remove(name);
        Ok(())
    }

    fn merge(&self, _: &str, into: &str, head: &str, _: &str) -> Result<Merged, ApiError> {
        let at = self
            .branch_at(into)
            .ok_or_else(|| ApiError::new(format!("no branch {into}")))?;
        let mut holds = self.holds.borrow()[&at].clone();
        if holds.iter().any(|h| h == head) {
            return Ok(Merged::Already);
        }
        if !holds.is_empty() && self.conflicts.contains(head) {
            return Ok(Merged::Conflict);
        }
        self.merges.borrow_mut().push((into.into(), head.into()));
        holds.push(head.into());
        let mut made = self.made.borrow_mut();
        *made += 1;
        let sha = format!("{:0>40x}", 0x00c0_ffee_0000_u64 + *made);
        let tree = tree_of(&holds);
        self.holds.borrow_mut().insert(sha.clone(), holds);
        self.branches.borrow_mut().insert(into.into(), sha.clone());
        Ok(Merged::Commit { sha, tree })
    }

    fn tree(&self, _: &str, sha: &str) -> Result<String, ApiError> {
        Ok(tree_of(&self.holds.borrow()[sha]))
    }
}

fn store(dir: &tempfile::TempDir) -> Store {
    let lock = conary_core::log::Lock::take(dir.path()).unwrap();
    Store::open(lock, SigningKey::from_bytes(&[5; 32]), None).unwrap()
}

fn join_all(store: &mut Store, hub: &Hub, prs: &[u64]) {
    for pr in prs {
        assert!(join(store, hub, &config(), &repo(), *pr, &head(*pr)).unwrap());
    }
}

fn step_once(store: &mut Store, hub: &Hub) -> Stepped {
    step(store, hub, hub, &config(), &repo(), &settings()).unwrap()
}

fn gating(stepped: &Stepped) -> (CandidateId, Vec<u64>) {
    match &stepped.now {
        Now::Gating { candidate, prs, .. } => (*candidate, prs.clone()),
        other => panic!("not gating: {other:?}"),
    }
}

/// The steward's landing of `pr`, as ingest would record it: GitHub's merge
/// of its head onto trunk, `after`, holding everything up to it.
fn land(store: &mut Store, hub: &Hub, pr: u64, after: &str) {
    let c = store.ledger().by_pr(&repo(), pr).unwrap();
    let revision = c.revisions.last().unwrap().id;
    let before = hub.branch_at("main").unwrap();
    let mut holds = hub.holds.borrow()[&before].clone();
    holds.push(head(pr));
    hub.holds.borrow_mut().insert(after.into(), holds);
    hub.branches
        .borrow_mut()
        .insert("main".into(), after.into());
    store
        .record(Body::Land {
            change: c.id,
            revision,
            before: before.parse().unwrap(),
            after: after.parse().unwrap(),
            by: Authority::Status {
                login: "fieldmouseworks-steward[bot]".into(),
            },
        })
        .unwrap();
}

#[test]
fn a_ready_pr_joins_at_its_head_as_github_shows_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new(&[3]);
    assert!(join(&mut store, &hub, &config(), &repo(), 3, &head(3)).unwrap());
    assert!(!join(&mut store, &hub, &config(), &repo(), 3, &head(3)).unwrap());
    // Settled at a head it's since moved on from.
    hub.pulls.borrow_mut().insert(3, head(4));
    match join(&mut store, &hub, &config(), &repo(), 3, &head(3)) {
        Err(TrainError::Moved { pr: 3, head: now }) => assert_eq!(now, head(4)),
        other => panic!("{other:?}"),
    }
    // The new head took it out.
    assert!(store.ledger().train(&repo()).unwrap().joined().is_empty());
    assert!(leave(&mut store, &repo(), 3, Left::Unready).is_ok_and(|left| !left));
}

#[test]
fn a_candidate_is_merged_on_its_build_branch_and_gated_on_its_train_branch() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new(&[3, 5, 4]);
    join_all(&mut store, &hub, &[3, 5, 4]);
    let stepped = step_once(&mut store, &hub);
    let (id, prs) = gating(&stepped);
    assert_eq!(prs, [3, 5, 4]);
    assert!(stepped.conflicts.is_empty());

    // Merged in the order they joined, onto trunk, on the build branch.
    let merges = hub.merges.borrow().clone();
    let build = build_branch(id);
    assert_eq!(
        merges,
        [3, 5, 4].map(|pr| (build.clone(), head(pr))).to_vec()
    );
    // The build branch is gone, and the train branch holds the result.
    let train = train_branch(id);
    assert_eq!(
        hub.names(),
        ["conary/train/".to_owned() + &id.to_string(), "main".into()]
    );
    assert_eq!(hub.held(&train), [3, 5, 4].map(head));
    let c = store.ledger().candidate(id).unwrap();
    assert_eq!(c.base.as_str(), TRUNK);
    assert_eq!(c.commit.as_str(), hub.branch_at(&train).unwrap());
    assert_eq!(c.tree.as_str(), tree_of(&[3, 5, 4].map(head)));
    assert_eq!(c.state, State::Gating);

    // Stepped again while it gates: nothing new is built.
    let again = step_once(&mut store, &hub);
    assert_eq!(gating(&again).0, id);
    assert_eq!(hub.merges.borrow().len(), 3);
}

#[test]
fn a_head_that_wont_merge_leaves_and_the_rest_are_built_without_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3, 4, 5]);
    hub.conflicts.insert(head(4));
    join_all(&mut store, &hub, &[3, 4, 5]);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(
        stepped.conflicts,
        [Conflict {
            pr: 4,
            onto: vec![3],
            base: TRUNK.parse().unwrap(),
        }]
    );
    let (id, prs) = gating(&stepped);
    assert_eq!(prs, [3, 5]);
    // Only the candidate's own branch is left.
    assert_eq!(hub.names(), [train_branch(id), "main".into()]);
    let joined: Vec<u64> = store
        .ledger()
        .train(&repo())
        .unwrap()
        .joined()
        .iter()
        .map(|m| store.ledger().change(m.change).unwrap().pr)
        .collect();
    assert_eq!(joined, [3, 5]);
}

#[test]
fn the_gates_results_are_recorded_for_the_candidates_commit_alone() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3]);
    join_all(&mut store, &hub, &[3]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    let commit = store.ledger().candidate(id).unwrap().commit.to_string();
    hub.checks.insert(
        commit.clone(),
        json!([
            {"name": "check / check", "status": "completed", "conclusion": "success",
             "html_url": "https://github.com/FieldmouseWorks/remi/actions/runs/1/job/1"},
            {"name": "slow", "status": "in_progress", "conclusion": null, "html_url": null},
        ]),
    );
    hub.statuses.insert(
        commit.clone(),
        json!([
            {"context": "review-settled", "state": "success", "target_url": null},
            {"context": "proof", "state": "failure", "target_url": "https://remi/proof/1"},
        ]),
    );
    // Something else pushed to the train branch counts for nothing.
    let pushed = format!("{:0>40}", "dead");
    hub.checks.insert(
        pushed.clone(),
        json!([{"name": "check / check", "status": "completed", "conclusion": "failure", "html_url": null}]),
    );
    hub.branches.borrow_mut().insert(train_branch(id), pushed);
    step_once(&mut store, &hub);
    step_once(&mut store, &hub);
    let evidence: Vec<(String, conary_core::op::Outcome)> = store
        .ledger()
        .candidate(id)
        .unwrap()
        .evidence
        .iter()
        .map(|e| (e.check.clone(), e.result))
        .collect();
    use conary_core::op::Outcome::{Fail, Pass};
    assert_eq!(
        evidence,
        [("check / check".into(), Pass), ("proof".into(), Fail)]
    );
}

#[test]
fn a_train_branch_lost_before_it_was_made_is_made_at_the_recorded_commit() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new(&[3]);
    join_all(&mut store, &hub, &[3]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    let commit = hub.branches.borrow_mut().remove(&train_branch(id)).unwrap();
    step_once(&mut store, &hub);
    assert_eq!(hub.branch_at(&train_branch(id)), Some(commit));
}

#[test]
fn green_lands_in_order_then_finishes_on_trunks_tree() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new(&[3, 4, 6]);
    join_all(&mut store, &hub, &[3, 4]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    verdict(&mut store, &repo(), id, Verdict::Green).unwrap();
    // One that joins now waits for the next candidate.
    join_all(&mut store, &hub, &[6]);
    let landings = match step_once(&mut store, &hub).now {
        Now::Landing {
            candidate,
            landings,
        } => {
            assert_eq!(candidate, id);
            landings
        }
        other => panic!("{other:?}"),
    };
    let order: Vec<(u64, String)> = landings
        .iter()
        .map(|l| (l.pr, l.head.to_string()))
        .collect();
    assert_eq!(order, [(3, head(3)), (4, head(4))]);

    land(&mut store, &hub, 3, &"a".repeat(40));
    match step_once(&mut store, &hub).now {
        Now::Landing { landings, .. } => assert_eq!(landings.len(), 1),
        other => panic!("{other:?}"),
    }
    land(&mut store, &hub, 4, &"b".repeat(40));
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.finished, [id]);
    let c = store.ledger().candidate(id).unwrap();
    assert!(c.same_tree(), "{:?}", c.state);
    // Its branch is gone, and the next candidate is the one that waited.
    let (next, prs) = gating(&stepped);
    assert_eq!(prs, [6]);
    assert_eq!(hub.names(), [train_branch(next), "main".into()]);
    assert_eq!(hub.held(&train_branch(next)), [3, 4, 6].map(head));
}

#[test]
fn trunk_that_moved_meanwhile_isnt_the_candidates_tree() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new(&[3, 9]);
    join_all(&mut store, &hub, &[3]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    verdict(&mut store, &repo(), id, Verdict::Green).unwrap();
    // Something else reached trunk first.
    let moved = "e".repeat(40);
    hub.holds.borrow_mut().insert(moved.clone(), vec![head(9)]);
    hub.branches.borrow_mut().insert("main".into(), moved);
    land(&mut store, &hub, 3, &"a".repeat(40));
    assert_eq!(step_once(&mut store, &hub).finished, [id]);
    assert!(!store.ledger().candidate(id).unwrap().same_tree());
}

#[test]
fn red_splits_and_the_older_half_is_built_on_trunk_as_it_is() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new(&[3, 4, 5, 6]);
    join_all(&mut store, &hub, &[3, 4, 5, 6]);
    let (whole, _) = gating(&step_once(&mut store, &hub));
    verdict(&mut store, &repo(), whole, Verdict::Red).unwrap();
    let stepped = step_once(&mut store, &hub);
    let (older, prs) = gating(&stepped);
    assert_eq!(prs, [3, 4]);
    assert_eq!(store.ledger().candidate(older).unwrap().of, Some(whole));
    // The red one's branch went.
    assert_eq!(hub.names(), [train_branch(older), "main".into()]);
    assert_eq!(hub.held(&train_branch(older)), [3, 4].map(head));
}

#[test]
fn stray_branches_go_and_only_the_trains_own() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new(&[]);
    for name in [
        "conary/build/left-by-a-stopped-step",
        "conary/train/of-a-candidate-done-with",
        "conary/notes-of-someone-else",
        "claude/x",
    ] {
        hub.branches.borrow_mut().insert(name.into(), TRUNK.into());
    }
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.now, Now::Idle);
    assert_eq!(
        hub.names(),
        ["claude/x", "conary/notes-of-someone-else", "main"]
    );
}

#[test]
fn a_verdict_is_only_for_its_own_repos_candidate() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new(&[3]);
    join_all(&mut store, &hub, &[3]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    let other: Repo = "FieldmouseWorks/burrow".parse().unwrap();
    assert!(matches!(
        verdict(&mut store, &other, id, Verdict::Green),
        Err(TrainError::NotHere(_))
    ));
    let unknown = CandidateId::new().unwrap();
    assert!(matches!(
        verdict(&mut store, &repo(), unknown, Verdict::Green),
        Err(TrainError::NotHere(_))
    ));
    verdict(&mut store, &repo(), id, Verdict::Dropped).unwrap();
    assert!(verdict(&mut store, &repo(), id, Verdict::Green).is_err());
}
