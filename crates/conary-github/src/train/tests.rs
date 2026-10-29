use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};

use super::*;
use crate::api::{CheckRun, Commit, PullRequest, Pulls, Review, Status as CommitStatus};

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
        reviewers: vec![STEWARD.into()],
        own_contexts: vec!["review-settled".into()],
    }
}

const STEWARD: &str = "fieldmouseworks-steward[bot]";

fn settings() -> Settings {
    Settings {
        trunk: "main".into(),
        size: 8,
        require: Vec::new(),
    }
}

/// GitHub, as far as the train can tell: PRs at their heads, each settled
/// by the steward, CI results by commit, and branches that merges really
/// move, each holding the heads merged into it. A head in `conflicts` won't
/// merge onto anything that holds another head. A PR's merge squashes its
/// head onto trunk.
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
    /// Each merged PR's commit on trunk.
    merged: RefCell<HashMap<u64, String>>,
    /// Each trunk commit's parent, and the PR it's linked to.
    parents: RefCell<HashMap<String, String>>,
    linked: RefCell<HashMap<String, u64>>,
    /// PRs whose merge GitHub refuses, and what it says.
    refuse: HashMap<u64, String>,
    /// GitHub not showing the merges it made yet.
    lagging: Cell<bool>,
    /// Every status the train posted.
    marks: RefCell<Vec<Posted>>,
}

#[derive(Debug, PartialEq, Eq)]
struct Posted {
    sha: String,
    mark: Mark,
    description: String,
    url: Option<String>,
}

impl Hub {
    fn new(prs: &[u64]) -> Self {
        let mut hub = Self::default();
        for pr in prs {
            hub.pulls.borrow_mut().insert(*pr, head(*pr));
            hub.statuses.insert(
                head(*pr),
                json!([{"context": "review-settled", "state": "success", "target_url": null,
                        "created_at": "2026-10-29T10:00:00Z", "creator": {"login": STEWARD, "type": "Bot"}}]),
            );
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

    /// CI on a commit: each check's name and conclusion, all completed.
    fn ci(&mut self, sha: &str, runs: &[(&str, &str)]) {
        let runs: Vec<Value> = runs
            .iter()
            .enumerate()
            .map(|(i, (name, conclusion))| {
                json!({"name": name, "status": "completed", "conclusion": conclusion,
                       "html_url": format!("https://github.com/FieldmouseWorks/remi/actions/runs/{i}"),
                       "started_at": format!("2026-10-29T11:0{i}:00Z")})
            })
            .collect();
        self.checks.insert(sha.into(), Value::Array(runs));
    }

    /// The trains's statuses, as (commit, mark).
    fn marked(&self) -> Vec<(String, Mark)> {
        self.marks
            .borrow()
            .iter()
            .map(|p| (p.sha.clone(), p.mark))
            .collect()
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
        let merged = self
            .merged
            .borrow()
            .get(&number)
            .filter(|_| !self.lagging.get())
            .cloned();
        Ok(parse(&json!({
            "number": number,
            "state": if merged.is_some() { "closed" } else { "open" },
            "title": "remi: a change",
            "body": "",
            "user": {"login": "TusanHomichi", "type": "User"},
            "merged_at": merged.as_ref().map(|_| "2026-10-29T12:00:00Z"),
            "merge_commit_sha": merged,
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

    fn commit(&self, _: &str, sha: &str) -> Result<Commit, ApiError> {
        let parent = self.parents.borrow()[sha].clone();
        Ok(parse(&json!({"sha": sha, "parents": [{"sha": parent}]})))
    }

    fn commit_pulls(&self, _: &str, sha: &str) -> Result<Vec<u64>, ApiError> {
        Ok(self.linked.borrow().get(sha).into_iter().copied().collect())
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

    fn mark(
        &self,
        _: &str,
        sha: &str,
        mark: Mark,
        description: &str,
        url: Option<&str>,
    ) -> Result<(), ApiError> {
        self.marks.borrow_mut().push(Posted {
            sha: sha.into(),
            mark,
            description: description.into(),
            url: url.map(str::to_owned),
        });
        Ok(())
    }

    fn land(&self, _: &str, pr: u64, at: &str) -> Result<PrMerge, ApiError> {
        if let Some(why) = self.refuse.get(&pr) {
            return Ok(PrMerge::Refused(why.clone()));
        }
        if self.pulls.borrow()[&pr] != at {
            return Ok(PrMerge::Refused("Head branch was modified".into()));
        }
        let before = self.branch_at("main").unwrap();
        let mut holds = self.holds.borrow()[&before].clone();
        holds.push(at.into());
        let mut made = self.made.borrow_mut();
        *made += 1;
        let sha = format!("{:0>40x}", 0x00ba_5e00_0000_u64 + *made);
        self.holds.borrow_mut().insert(sha.clone(), holds);
        self.branches
            .borrow_mut()
            .insert("main".into(), sha.clone());
        self.parents.borrow_mut().insert(sha.clone(), before);
        self.linked.borrow_mut().insert(sha.clone(), pr);
        self.merged.borrow_mut().insert(pr, sha);
        Ok(PrMerge::Merged)
    }
}

fn store(dir: &tempfile::TempDir) -> Store {
    let lock = conary_core::log::Lock::take(dir.path()).unwrap();
    Store::open(lock, SigningKey::from_bytes(&[5; 32]), None).unwrap()
}

fn join_all(store: &mut Store, hub: &Hub, prs: &[u64]) {
    for pr in prs {
        assert_eq!(
            join(store, hub, &config(), &repo(), *pr, &head(*pr), false).unwrap(),
            Joined::Joined
        );
    }
}

fn joined(store: &Store) -> Vec<u64> {
    store
        .ledger()
        .train(&repo())
        .map(|t| {
            t.joined()
                .iter()
                .map(|m| store.ledger().change(m.change).unwrap().pr)
                .collect()
        })
        .unwrap_or_default()
}

fn commit_of(store: &Store, id: CandidateId) -> String {
    store.ledger().candidate(id).unwrap().commit.to_string()
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

fn waiting(stepped: &Stepped) -> Vec<String> {
    match &stepped.now {
        Now::Gating { waiting, .. } => waiting.clone(),
        other => panic!("not gating: {other:?}"),
    }
}

#[test]
fn a_ready_pr_joins_at_its_head_as_github_shows_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new(&[3]);
    let mut again = || join(&mut store, &hub, &config(), &repo(), 3, &head(3), false).unwrap();
    assert_eq!(again(), Joined::Joined);
    assert_eq!(again(), Joined::Already);
    // Settled at a head it's since moved on from.
    hub.pulls.borrow_mut().insert(3, head(4));
    match join(&mut store, &hub, &config(), &repo(), 3, &head(3), false) {
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
    assert_eq!(joined(&store), [3, 5]);
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
fn green_lands_in_order_with_the_trains_status_then_finishes_on_trunks_tree() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3, 4, 6]);
    join_all(&mut store, &hub, &[3, 4]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    // One that joins now waits for the next candidate.
    join_all(&mut store, &hub, &[6]);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.verdicts, [(id, Verdict::Green)]);
    assert_eq!(stepped.landed, [3, 4]);
    // Each head got the train's word before its merge, in order.
    assert_eq!(
        hub.marked(),
        [(head(3), Mark::Landing), (head(4), Mark::Landing)]
    );
    assert_eq!(hub.held("main"), [3, 4].map(head));
    for pr in [3, 4] {
        let c = store.ledger().by_pr(&repo(), pr).unwrap();
        let landing = c.landing.as_ref().unwrap();
        assert_eq!(landing.candidate, Some(id));
        assert_eq!(landing.after.to_string(), hub.merged.borrow()[&pr]);
    }
    assert_eq!(stepped.finished, [id]);
    assert!(store.ledger().candidate(id).unwrap().same_tree());
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
    let mut hub = Hub::new(&[3, 9]);
    join_all(&mut store, &hub, &[3]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    // Something else reached trunk first.
    let moved = "e".repeat(40);
    hub.holds.borrow_mut().insert(moved.clone(), vec![head(9)]);
    hub.branches.borrow_mut().insert("main".into(), moved);
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    let stepped = step_once(&mut store, &hub);
    assert_eq!((stepped.landed, stepped.finished), (vec![3], vec![id]));
    assert!(!store.ledger().candidate(id).unwrap().same_tree());
}

#[test]
fn a_pr_alone_lands_only_while_trunk_is_its_base_and_is_built_again_once_it_moved() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3, 4, 9]);
    join_all(&mut store, &hub, &[3]);
    assert_eq!(
        join(&mut store, &hub, &config(), &repo(), 4, &head(4), true).unwrap(),
        Joined::Joined
    );
    // The PR ahead of it goes without it.
    let (first, prs) = gating(&step_once(&mut store, &hub));
    assert_eq!(prs, [3]);
    hub.ci(&commit_of(&store, first), &[("check / check", "success")]);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.landed, [3]);
    // Then it's built by itself, on trunk as that landing left it.
    let (id, prs) = gating(&stepped);
    assert_eq!(prs, [4]);
    assert!(store.ledger().candidate(id).unwrap().alone);

    // Something else reached trunk before it landed: its green is for a
    // tree it won't make, so it's dropped and built again on trunk as it is.
    let moved = "e".repeat(40);
    hub.holds
        .borrow_mut()
        .insert(moved.clone(), [3, 9].map(head).to_vec());
    hub.branches
        .borrow_mut()
        .insert("main".into(), moved.clone());
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(
        stepped.verdicts,
        [(id, Verdict::Green), (id, Verdict::Dropped)]
    );
    assert!(stepped.landed.is_empty());
    assert!(!hub.marked().contains(&(head(4), Mark::Landing)));
    let (again, prs) = gating(&stepped);
    assert_eq!(prs, [4]);
    let c = store.ledger().candidate(again).unwrap();
    assert_eq!(c.base.to_string(), moved);

    // On the trunk it was built on, it lands, on its own tree.
    hub.ci(&commit_of(&store, again), &[("check / check", "success")]);
    let stepped = step_once(&mut store, &hub);
    assert_eq!((stepped.landed, stepped.finished), (vec![4], vec![again]));
    assert!(store.ledger().candidate(again).unwrap().same_tree());
}

#[test]
fn the_gate_is_green_only_once_every_check_passed_and_the_required_ones_reported() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3]);
    join_all(&mut store, &hub, &[3]);
    let required = Settings {
        require: vec!["proof".into()],
        ..settings()
    };
    let step_req = |store: &mut Store, hub: &Hub| {
        step(store, hub, hub, &config(), &repo(), &required).unwrap()
    };
    // No check at all is no pass, and names nothing to wait on but what's
    // required.
    let stepped = step_once(&mut store, &hub);
    let (id, _) = gating(&stepped);
    assert!(waiting(&stepped).is_empty());
    assert_eq!(waiting(&step_req(&mut store, &hub)), ["proof"]);
    let commit = commit_of(&store, id);
    hub.checks.insert(
        commit.clone(),
        json!([
            {"name": "check / check", "status": "completed", "conclusion": "success",
             "html_url": null, "started_at": "2026-10-29T11:00:00Z"},
            {"name": "slow", "status": "in_progress", "conclusion": null,
             "html_url": null, "started_at": "2026-10-29T11:00:00Z"},
        ]),
    );
    assert_eq!(waiting(&step_req(&mut store, &hub)), ["slow", "proof"]);
    hub.ci(
        &commit,
        &[("check / check", "success"), ("slow", "skipped")],
    );
    // The proof job hasn't posted, and the steward's own word isn't a check.
    hub.statuses.insert(
        commit.clone(),
        json!([{"context": "review-settled", "state": "success", "target_url": null},
               {"context": CONTEXT, "state": "pending", "target_url": null}]),
    );
    assert_eq!(waiting(&step_req(&mut store, &hub)), ["proof"]);
    hub.statuses.insert(
        commit.clone(),
        json!([{"context": "proof", "state": "pending", "target_url": null}]),
    );
    assert_eq!(waiting(&step_req(&mut store, &hub)), ["proof"]);
    hub.statuses.insert(
        commit,
        json!([{"context": "proof", "state": "success", "target_url": null},
               {"context": "proof", "state": "pending", "target_url": null}]),
    );
    let stepped = step_req(&mut store, &hub);
    assert_eq!(stepped.verdicts, [(id, Verdict::Green)]);
    assert_eq!(stepped.landed, [3]);
}

#[test]
fn each_checks_latest_run_is_its_word() {
    // A rerun's runs come in no order the train can count on.
    let rerun = |first: &str, then: &str, newest_first: bool| {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(&dir);
        let mut hub = Hub::new(&[3]);
        join_all(&mut store, &hub, &[3]);
        let (id, _) = gating(&step_once(&mut store, &hub));
        let mut runs = vec![
            json!({"name": "check", "status": "completed", "conclusion": first,
                   "html_url": null, "started_at": "2026-10-29T11:00:00Z"}),
            json!({"name": "check", "status": "completed", "conclusion": then,
                   "html_url": null, "started_at": "2026-10-29T11:30:00Z"}),
        ];
        if newest_first {
            runs.reverse();
        }
        hub.checks.insert(commit_of(&store, id), Value::Array(runs));
        step_once(&mut store, &hub).verdicts[0].1
    };
    for newest_first in [true, false] {
        assert_eq!(rerun("failure", "success", newest_first), Verdict::Green);
        assert_eq!(rerun("success", "failure", newest_first), Verdict::Red);
    }
}

#[test]
fn red_alone_leaves_with_the_trains_failure_and_stays_out_until_its_head_moves() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3]);
    join_all(&mut store, &hub, &[3]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(
        &commit_of(&store, id),
        &[("check / check", "failure"), ("lint", "success")],
    );
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.verdicts, [(id, Verdict::Red)]);
    let url = "https://github.com/FieldmouseWorks/remi/actions/runs/0".to_owned();
    assert_eq!(
        stepped.red,
        [Red {
            pr: 3,
            candidate: id,
            failed: vec![Failed {
                check: "check / check".into(),
                url: Some(url.clone()),
            }],
        }]
    );
    assert_eq!(
        *hub.marks.borrow(),
        [Posted {
            sha: head(3),
            mark: Mark::Red,
            description: format!("red alone in candidate {id}: check / check"),
            url: Some(url),
        }]
    );
    assert_eq!(stepped.now, Now::Idle);
    assert_eq!(
        join(&mut store, &hub, &config(), &repo(), 3, &head(3), false).unwrap(),
        Joined::Held(Held::Red)
    );
    hub.pulls.borrow_mut().insert(3, head(33));
    assert_eq!(
        join(&mut store, &hub, &config(), &repo(), 3, &head(33), false).unwrap(),
        Joined::Joined
    );
}

#[test]
fn red_with_others_splits_and_builds_the_older_half_in_the_same_step() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3, 4, 5]);
    join_all(&mut store, &hub, &[3, 4, 5]);
    let (whole, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, whole), &[("check / check", "failure")]);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.verdicts, [(whole, Verdict::Red)]);
    assert!(stepped.red.is_empty() && hub.marked().is_empty());
    let (older, prs) = gating(&stepped);
    assert_eq!(prs, [3, 4]);
    assert_eq!(store.ledger().candidate(older).unwrap().of, Some(whole));
}

#[test]
fn a_refused_merge_leaves_and_the_changes_after_it_go_again() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3, 4, 5]);
    hub.refuse.insert(
        4,
        "Required status check \"review-settled\" is expected.".into(),
    );
    join_all(&mut store, &hub, &[3, 4, 5]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.landed, [3]);
    assert_eq!(
        stepped.refused,
        [Refused {
            pr: 4,
            why: "Required status check \"review-settled\" is expected.".into(),
        }]
    );
    assert_eq!(
        store.ledger().candidate(id).unwrap().state,
        State::Broken {
            by: store.ledger().by_pr(&repo(), 4).unwrap().id
        }
    );
    // Its head said it was landing, and says now that it isn't.
    assert_eq!(
        hub.marked(),
        [
            (head(3), Mark::Landing),
            (head(4), Mark::Landing),
            (head(4), Mark::Refused)
        ]
    );
    // 5 is built again, on trunk with 3, without 4.
    let (next, prs) = gating(&stepped);
    assert_eq!(prs, [5]);
    assert_eq!(hub.held(&train_branch(next)), [3, 5].map(head));
    assert_eq!(
        join(&mut store, &hub, &config(), &repo(), 4, &head(4), false).unwrap(),
        Joined::Held(Held::Refused)
    );
    // Once trunk moves, it can try again.
    hub.ci(&commit_of(&store, next), &[("check / check", "success")]);
    hub.refuse.clear();
    assert_eq!(step_once(&mut store, &hub).landed, [5]);
    join_all(&mut store, &hub, &[4]);
}

#[test]
fn a_merge_github_hasnt_shown_yet_waits_for_the_next_step() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3, 4]);
    join_all(&mut store, &hub, &[3, 4]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    hub.lagging.set(true);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.landed, [3]);
    match &stepped.now {
        Now::Landing {
            candidate,
            landings,
        } => {
            assert_eq!(*candidate, id);
            let prs: Vec<u64> = landings.iter().map(|l| l.pr).collect();
            assert_eq!(prs, [3, 4]);
        }
        other => panic!("{other:?}"),
    }
    // Its merge's event: the merge is read, never made again.
    hub.lagging.set(false);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.landed, [4]);
    assert_eq!(stepped.finished, [id]);
    assert_eq!(hub.held("main"), [3, 4].map(head));
}

#[test]
fn a_pr_that_wont_merge_is_held_until_what_it_was_after_moves() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3, 4]);
    hub.conflicts.insert(head(4));
    join_all(&mut store, &hub, &[3, 4]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    assert_eq!(
        join(&mut store, &hub, &config(), &repo(), 4, &head(4), false).unwrap(),
        Joined::Held(Held::Conflict)
    );
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    assert_eq!(step_once(&mut store, &hub).landed, [3]);
    // Trunk moved: it goes again, and won't merge onto trunk itself now.
    join_all(&mut store, &hub, &[4]);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(
        stepped.conflicts,
        [Conflict {
            pr: 4,
            onto: vec![],
            base: hub.branch_at("main").unwrap().parse().unwrap(),
        }]
    );
    assert_eq!(stepped.now, Now::Idle);
    assert!(joined(&store).is_empty());
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
