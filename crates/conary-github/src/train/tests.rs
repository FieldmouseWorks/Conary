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
        size: 8,
        require: Vec::new(),
    }
}

/// GitHub, as far as the train can tell: PRs at their heads, each settled
/// by the steward, CI results by commit, and branches that merges really
/// move, each holding the heads merged into it. A head in `conflicts` won't
/// merge onto anything that holds another head, and one in `same` changes
/// just what another head does, so merged where that one is already it
/// leaves the tree as it was. A head in `stacked` is on another's branch:
/// merging it, or landing it, brings that head too. A PR's merge squashes
/// its head onto trunk.
#[derive(Default)]
struct Hub {
    /// The repo's default branch.
    default: RefCell<String>,
    pulls: RefCell<BTreeMap<u64, String>>,
    checks: HashMap<String, Value>,
    statuses: HashMap<String, Value>,
    conflicts: HashSet<String>,
    /// A head that does what the head it maps to does.
    same: HashMap<String, String>,
    /// A head and the head of the PR its branch was made on.
    stacked: HashMap<String, String>,
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
    /// GitHub showing them, but not linking their commits to their PRs yet.
    unlinked: Cell<bool>,
    /// A PR's merge failing before GitHub makes it, or once it's made with
    /// GitHub's answer lost.
    down: Cell<bool>,
    lost: Cell<bool>,
    /// Statuses of this kind GitHub doesn't take.
    unmarked: Cell<Option<Mark>>,
    /// Each PR's base branch, when it isn't the default one.
    targets: RefCell<HashMap<u64, String>>,
    /// A PR retargeted the moment its head says it's landing.
    retarget: RefCell<Option<(u64, String)>>,
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
        hub.default.replace("main".into());
        hub.branches
            .borrow_mut()
            .insert("main".into(), TRUNK.into());
        hub.holds.borrow_mut().insert(TRUNK.into(), Vec::new());
        hub
    }

    /// The branch a PR targets: the default one unless it was told another.
    /// The tree of a commit holding these heads: what they do, each once.
    fn tree_holding(&self, holds: &[String]) -> String {
        let mut does: Vec<String> = Vec::new();
        for h in holds {
            let d = self.same.get(h).unwrap_or(h);
            if !does.contains(d) {
                does.push(d.clone());
            }
        }
        tree_of(&does)
    }

    fn base_of(&self, pr: u64) -> String {
        self.targets
            .borrow()
            .get(&pr)
            .cloned()
            .unwrap_or_else(|| self.default.borrow().clone())
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
                json!({"id": i + 1, "name": name, "status": "completed", "conclusion": conclusion,
                       "html_url": format!("https://github.com/FieldmouseWorks/remi/actions/runs/{i}"),
                       "started_at": format!("2026-10-29T11:0{i}:00Z")})
            })
            .collect();
        self.checks.insert(sha.into(), Value::Array(runs));
    }

    /// What the train's statuses said became of each head, as (commit,
    /// mark): landing, red or refused, the pending ones aside.
    fn marked(&self) -> Vec<(String, Mark)> {
        self.marks
            .borrow()
            .iter()
            .filter(|p| p.mark != Mark::Waiting)
            .map(|p| (p.sha.clone(), p.mark))
            .collect()
    }

    /// The train's latest status on a commit.
    fn latest(&self, sha: &str) -> Option<Mark> {
        self.marks
            .borrow()
            .iter()
            .rev()
            .find(|p| p.sha == sha)
            .map(|p| p.mark)
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
        let base = self.base_of(number);
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
            "base": {"sha": TRUNK, "ref": base,
                     "repo": {"default_branch": *self.default.borrow()}},
            "head": {"sha": head, "ref": "claude/x"},
        })))
    }

    fn pulls(&self, _: &str, _: Pulls) -> Result<Vec<PullRequest>, ApiError> {
        unreachable!("the train reads one PR at a time")
    }

    fn check_runs(&self, _: &str, sha: &str) -> Result<Vec<CheckRun>, ApiError> {
        Ok(self.checks.get(sha).map_or_else(Vec::new, parse))
    }

    /// The train's own, newest first, then the rest.
    fn statuses(&self, _: &str, sha: &str) -> Result<Vec<CommitStatus>, ApiError> {
        let state = |m: Mark| match m {
            Mark::Landing => "success",
            Mark::Waiting => "pending",
            Mark::Red => "failure",
            Mark::Refused => "error",
        };
        let mut all: Vec<CommitStatus> = self
            .marks
            .borrow()
            .iter()
            .rev()
            .filter(|p| p.sha == sha)
            .map(|p| {
                parse(&json!({"context": CONTEXT, "state": state(p.mark),
                              "description": p.description, "target_url": p.url}))
            })
            .collect();
        all.extend(
            self.statuses
                .get(sha)
                .map_or_else(Vec::new, parse::<Vec<CommitStatus>>),
        );
        Ok(all)
    }

    fn reviews(&self, _: &str, _: u64) -> Result<Vec<Review>, ApiError> {
        Ok(Vec::new())
    }

    fn commit(&self, _: &str, sha: &str) -> Result<Commit, ApiError> {
        let parent = self.parents.borrow()[sha].clone();
        Ok(parse(&json!({"sha": sha, "parents": [{"sha": parent}]})))
    }

    fn commit_pulls(&self, _: &str, sha: &str) -> Result<Vec<u64>, ApiError> {
        Ok(self
            .linked
            .borrow()
            .get(sha)
            .filter(|_| !self.unlinked.get())
            .into_iter()
            .copied()
            .collect())
    }

    fn retargeted(&self, _: &str, _: u64) -> Result<Option<String>, ApiError> {
        Ok(None)
    }

    fn merge_base(&self, _: &str, _: &str, _: &str) -> Result<String, ApiError> {
        Ok(FORK.into())
    }
}

impl Forge for Hub {
    fn default_branch(&self, _: &str) -> Result<String, ApiError> {
        Ok(self.default.borrow().clone())
    }

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
        holds.extend(self.stacked.get(head).cloned());
        holds.push(head.into());
        let mut made = self.made.borrow_mut();
        *made += 1;
        let sha = format!("{:0>40x}", 0x00c0_ffee_0000_u64 + *made);
        let tree = self.tree_holding(&holds);
        self.holds.borrow_mut().insert(sha.clone(), holds);
        self.branches.borrow_mut().insert(into.into(), sha.clone());
        Ok(Merged::Commit { sha, tree })
    }

    fn tree(&self, _: &str, sha: &str) -> Result<String, ApiError> {
        Ok(self.tree_holding(&self.holds.borrow()[sha]))
    }

    fn mark(
        &self,
        _: &str,
        sha: &str,
        mark: Mark,
        description: &str,
        url: Option<&str>,
    ) -> Result<(), ApiError> {
        if self.unmarked.get() == Some(mark) {
            return Err(ApiError::unavailable("statuses"));
        }
        if mark == Mark::Landing
            && let Some((pr, base)) = self.retarget.take()
        {
            self.targets.borrow_mut().insert(pr, base);
        }
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
        if self.merged.borrow().contains_key(&pr) {
            return Ok(PrMerge::Refused("Pull Request is not mergeable".into()));
        }
        if self.down.get() {
            return Err(ApiError::unavailable("merge"));
        }
        let base = self.base_of(pr);
        let before = self.branch_at(&base).unwrap();
        let mut holds = self.holds.borrow()[&before].clone();
        holds.extend(self.stacked.get(at).cloned());
        holds.push(at.into());
        let mut made = self.made.borrow_mut();
        *made += 1;
        let sha = format!("{:0>40x}", 0x00ba_5e00_0000_u64 + *made);
        self.holds.borrow_mut().insert(sha.clone(), holds);
        self.branches.borrow_mut().insert(base, sha.clone());
        self.parents.borrow_mut().insert(sha.clone(), before);
        self.linked.borrow_mut().insert(sha.clone(), pr);
        self.merged.borrow_mut().insert(pr, sha);
        if self.lost.get() {
            return Err(ApiError::unavailable("merge"));
        }
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
            join(store, hub, hub, &config(), &repo(), *pr, &head(*pr), false).unwrap(),
            Joined::Joined
        );
    }
}

/// Joins PR `pr` at `at`, as the steward asks once it's settled there.
fn try_join(
    store: &mut Store,
    hub: &Hub,
    pr: u64,
    at: &str,
    alone: bool,
) -> Result<Joined, TrainError> {
    join(store, hub, hub, &config(), &repo(), pr, at, alone)
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

mod empty;
mod land;

#[test]
fn a_ready_pr_joins_at_its_head_as_github_shows_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new(&[3]);
    let mut again = || try_join(&mut store, &hub, 3, &head(3), false).unwrap();
    assert_eq!(again(), Joined::Joined);
    assert_eq!(again(), Joined::Already);
    // Settled at a head it's since moved on from.
    hub.pulls.borrow_mut().insert(3, head(4));
    match try_join(&mut store, &hub, 3, &head(3), false) {
        Err(TrainError::Moved { pr: 3, head: now }) => assert_eq!(now, head(4)),
        other => panic!("{other:?}"),
    }
    // The new head took it out.
    assert!(store.ledger().train(&repo()).unwrap().joined().is_empty());
    assert!(
        leave(&mut store, &hub, &hub, &repo(), 3, Left::Unready)
            .is_ok_and(|left| left == Leaving::NotIn)
    );
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
            {"id": 2, "name": "check / check", "status": "completed", "conclusion": "success",
             "html_url": "https://github.com/FieldmouseWorks/remi/actions/runs/1/job/1"},
            {"id": 3, "name": "slow", "status": "in_progress", "conclusion": null, "html_url": null},
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
        json!([{"id": 4, "name": "check / check", "status": "completed", "conclusion": "failure", "html_url": null}]),
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
            {"id": 5, "name": "check / check", "status": "completed", "conclusion": "success",
             "html_url": null, "started_at": "2026-10-29T11:00:00Z"},
            {"id": 6, "name": "slow", "status": "in_progress", "conclusion": null,
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
    // The verdict on a candidate whose commit has these runs and statuses,
    // or none while it waits.
    let gate = |runs: Vec<Value>, statuses: Value| {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(&dir);
        let mut hub = Hub::new(&[3]);
        join_all(&mut store, &hub, &[3]);
        let (id, _) = gating(&step_once(&mut store, &hub));
        let commit = commit_of(&store, id);
        hub.checks.insert(commit.clone(), Value::Array(runs));
        hub.statuses.insert(commit, statuses);
        step_once(&mut store, &hub).verdicts.first().map(|v| v.1)
    };
    // A run of `check` in a suite: done with `conclusion`, else queued.
    let run = |id: u64, suite: u64, conclusion: Option<&str>, started: Option<&str>| {
        json!({"id": id, "name": "check", "check_suite": {"id": suite}, "html_url": null,
               "status": if conclusion.is_some() { "completed" } else { "queued" },
               "conclusion": conclusion, "started_at": started})
    };
    let at = Some("2026-10-29T11:00:00Z");

    // A rerun's runs come in no order the train can count on: the later
    // run is the check's word.
    for (first, then, verdict) in [
        ("failure", "success", Verdict::Green),
        ("success", "failure", Verdict::Red),
    ] {
        let mut runs = vec![run(7, 1, Some(first), at), run(8, 1, Some(then), at)];
        assert_eq!(gate(runs.clone(), json!([])), Some(verdict));
        runs.reverse();
        assert_eq!(gate(runs, json!([])), Some(verdict));
    }
    // Whether it started says nothing about which is later: an earlier
    // skipped run that never started doesn't hide the rerun that failed,
    // and a rerun still queued keeps the gate waiting.
    let skipped_then_failed = vec![
        run(7, 1, Some("skipped"), None),
        run(8, 1, Some("failure"), at),
    ];
    assert_eq!(gate(skipped_then_failed, json!([])), Some(Verdict::Red));
    let failed_then_queued = vec![run(7, 1, Some("failure"), at), run(8, 1, None, None)];
    assert_eq!(gate(failed_then_queued, json!([])), None);
    // Runs of one name in two suites (two workflows, or two Apps) are two
    // checks, and a status and a run of one name are too.
    for (a, b) in [("success", "failure"), ("failure", "success")] {
        let runs = vec![run(7, 1, Some(a), at), run(8, 2, Some(b), at)];
        assert_eq!(gate(runs, json!([])), Some(Verdict::Red));
    }
    let failed = json!([{"context": "check", "state": "failure", "target_url": null}]);
    assert_eq!(
        gate(vec![run(7, 1, Some("success"), at)], failed),
        Some(Verdict::Red)
    );
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
        hub.marks.borrow().last(),
        Some(&Posted {
            sha: head(3),
            mark: Mark::Red,
            description: format!("red alone in candidate {id}: check / check"),
            url: Some(url),
        })
    );
    assert_eq!(stepped.now, Now::Idle);
    assert_eq!(
        try_join(&mut store, &hub, 3, &head(3), false).unwrap(),
        Joined::Held(Held::Red)
    );
    hub.pulls.borrow_mut().insert(3, head(33));
    assert_eq!(
        try_join(&mut store, &hub, 3, &head(33), false).unwrap(),
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
fn red_alone_is_recorded_only_once_its_head_has_the_failure() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3]);
    join_all(&mut store, &hub, &[3]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "failure")]);
    hub.unmarked.set(Some(Mark::Red));
    assert!(step(&mut store, &hub, &hub, &config(), &repo(), &settings()).is_err());
    assert_eq!(store.ledger().candidate(id).unwrap().state, State::Gating);
    hub.unmarked.set(None);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.verdicts, [(id, Verdict::Red)]);
    assert_eq!(hub.marked(), [(head(3), Mark::Red)]);
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
        try_join(&mut store, &hub, 4, &head(4), false).unwrap(),
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
    store
        .record(Body::Verdict {
            candidate: whole,
            verdict: Verdict::Red,
        })
        .unwrap();
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
fn a_candidate_is_dropped_only_in_its_own_repo_and_only_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new(&[3]);
    join_all(&mut store, &hub, &[3]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    let other: Repo = "FieldmouseWorks/burrow".parse().unwrap();
    assert!(matches!(
        drop_candidate(&mut store, &hub, &hub, &other, id),
        Err(TrainError::NotHere(_))
    ));
    let unknown = CandidateId::new().unwrap();
    assert!(matches!(
        drop_candidate(&mut store, &hub, &hub, &repo(), unknown),
        Err(TrainError::NotHere(_))
    ));
    drop_candidate(&mut store, &hub, &hub, &repo(), id).unwrap();
    assert!(drop_candidate(&mut store, &hub, &hub, &repo(), id).is_err());
    assert_eq!(joined(&store), [3]);
}
