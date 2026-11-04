use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};

use super::*;
use crate::api::{CheckRun, Commit, Event, PullRequest, Pulls, Review, Status};
use conary_core::ledger::Status as Kept;
use conary_core::op::{Actor, ActorKind, Authority};

/// Trunk before the change landed, the change's landing, and trunk now.
const BEFORE: &str = "1111111111111111111111111111111111111111";
const AFTER: &str = "2222222222222222222222222222222222222222";
const TRUNK: &str = "3333333333333333333333333333333333333333";
const STEWARD: &str = "fieldmouseworks-steward[bot]";

fn repo() -> Repo {
    "FieldmouseWorks/remi".parse().unwrap()
}

fn config() -> Config {
    Config {
        reviewers: vec![STEWARD.into()],
        own_contexts: vec!["review-settled".into()],
    }
}

/// GitHub, as far as undo can tell: commits with their trees and
/// parents, branches, and the PRs opened from them. Merging trunk into a
/// revert's branch makes `merging`'s tree, or conflicts when it's `None`;
/// a branch that holds trunk already is merged already. Merging another
/// revert in makes a tree of both. A revert whose tree is `gone` changes
/// nothing: trunk already lacks what its change brought.
struct Hub {
    trees: RefCell<HashMap<String, String>>,
    parents: RefCell<HashMap<String, Vec<String>>>,
    branches: RefCell<BTreeMap<String, String>>,
    merging: RefCell<Option<String>>,
    /// Trees whose commit conflicts when it's merged into a branch.
    conflicting: RefCell<Vec<String>>,
    /// Trees of reverts that change nothing.
    gone: RefCell<Vec<String>>,
    /// Every commit made, as (tree, parent, message).
    made: RefCell<Vec<(String, String, String)>>,
    pulls: RefCell<Vec<Value>>,
    /// The PRs each trunk commit a merge made is linked to.
    linked: RefCell<HashMap<String, Vec<u64>>>,
    /// The heads the steward settled, before they merged.
    settled: RefCell<Vec<String>>,
    /// When each PR closed and reopened, oldest first.
    turns: RefCell<HashMap<u64, Vec<(String, &'static str)>>>,
    /// Repos GitHub errs for.
    down: RefCell<Vec<String>>,
    n: Cell<u64>,
    /// Seconds since noon, a tick for each thing that happens.
    clock: Cell<u64>,
}

impl Hub {
    fn new() -> Self {
        let commits = [(BEFORE, "before"), (AFTER, "after"), (TRUNK, "trunk")];
        Self {
            trees: RefCell::new(
                commits
                    .iter()
                    .map(|(c, t)| (c.to_string(), t.to_string()))
                    .collect(),
            ),
            parents: RefCell::new(HashMap::from([
                (AFTER.into(), vec![BEFORE.into()]),
                (TRUNK.into(), vec![AFTER.into()]),
            ])),
            branches: RefCell::new(BTreeMap::from([("main".into(), TRUNK.into())])),
            merging: RefCell::new(Some("trunk without the change".into())),
            conflicting: RefCell::new(Vec::new()),
            gone: RefCell::new(Vec::new()),
            made: RefCell::new(Vec::new()),
            pulls: RefCell::new(Vec::new()),
            linked: RefCell::new(HashMap::new()),
            settled: RefCell::new(Vec::new()),
            turns: RefCell::new(HashMap::new()),
            down: RefCell::new(Vec::new()),
            n: Cell::new(0),
            clock: Cell::new(0),
        }
    }

    fn now(&self) -> String {
        let t = self.clock.get() + 1;
        self.clock.set(t);
        format!("2026-10-27T12:{:02}:{:02}Z", t / 60, t % 60)
    }

    fn turn(&self, number: u64, state: &str, event: &'static str) {
        let at = self.now();
        for p in self.pulls.borrow_mut().iter_mut() {
            if p["number"] == number {
                p["state"] = json!(state);
            }
        }
        let mut turns = self.turns.borrow_mut();
        turns.entry(number).or_default().push((at, event));
    }

    fn sha(&self) -> String {
        self.n.set(self.n.get() + 1);
        format!("{:0>40x}", 0xc0ffee_u64 + self.n.get())
    }

    /// Whether `commit` is `at` or one of its ancestors.
    fn holds(&self, at: &str, commit: &str) -> bool {
        let parents = self.parents.borrow();
        let mut todo = vec![at.to_owned()];
        while let Some(c) = todo.pop() {
            if c == commit {
                return true;
            }
            todo.extend(parents.get(&c).into_iter().flatten().cloned());
        }
        false
    }

    fn parent(&self, of: &str) -> String {
        self.parents.borrow()[of][0].clone()
    }

    fn pull_json(&self, number: u64) -> Value {
        self.pulls
            .borrow()
            .iter()
            .find(|p| p["number"] == number)
            .cloned()
            .unwrap()
    }

    fn close(&self, number: u64) {
        self.turn(number, "closed", "closed");
    }

    fn reopen(&self, number: u64) {
        self.turn(number, "open", "reopened");
    }

    /// The steward lands PR `number`, settled, squashed onto trunk, and
    /// its branch goes: trunk's new tip.
    fn land(&self, number: u64) -> String {
        let sha = self.sha();
        let trunk = self.branches.borrow()["main"].clone();
        let mut pulls = self.pulls.borrow_mut();
        let p = pulls.iter_mut().find(|p| p["number"] == number).unwrap();
        let head = p["head"]["sha"].as_str().unwrap().to_owned();
        let tree = self.trees.borrow()[&head].clone();
        self.trees.borrow_mut().insert(sha.clone(), tree);
        self.parents.borrow_mut().insert(sha.clone(), vec![trunk]);
        self.linked.borrow_mut().insert(sha.clone(), vec![number]);
        self.settled.borrow_mut().push(head);
        self.branches
            .borrow_mut()
            .insert("main".into(), sha.clone());
        let branch = p["head"]["ref"].as_str().unwrap().to_owned();
        self.branches.borrow_mut().remove(&branch);
        p["merged_at"] = json!(self.now());
        p["merge_commit_sha"] = json!(sha);
        drop(pulls);
        self.turn(number, "closed", "closed");
        sha
    }
}

impl Branches for Hub {
    fn default_branch(&self, repo: &str) -> Result<String, ApiError> {
        if self.down.borrow().iter().any(|d| d == repo) {
            return Err(ApiError::new(format!("{repo}: Server Error")));
        }
        Ok("main".into())
    }

    fn branch(&self, _: &str, name: &str) -> Result<Option<String>, ApiError> {
        Ok(self.branches.borrow().get(name).cloned())
    }

    fn branches(&self, _: &str, prefix: &str) -> Result<Vec<String>, ApiError> {
        Ok(self
            .branches
            .borrow()
            .keys()
            .filter(|n| n.starts_with(prefix))
            .cloned()
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
        let at = self.branches.borrow()[into].clone();
        if self.holds(&at, head) {
            return Ok(Merged::Already);
        }
        let Some(merging) = self.merging.borrow().clone() else {
            return Ok(Merged::Conflict);
        };
        let (here, there) = {
            let trees = self.trees.borrow();
            (trees[&at].clone(), trees[head].clone())
        };
        if self.conflicting.borrow().contains(&there) {
            return Ok(Merged::Conflict);
        }
        let trunk = head == self.branches.borrow()["main"];
        let gone = self.gone.borrow();
        let tree = match (trunk, gone.contains(&here)) {
            (true, true) => there,
            (true, false) => merging,
            (false, _) if gone.contains(&there) => here,
            (false, _) => format!("{here}, then {there}"),
        };
        drop(gone);
        let sha = self.sha();
        self.trees.borrow_mut().insert(sha.clone(), tree.clone());
        self.parents
            .borrow_mut()
            .insert(sha.clone(), vec![at, head.into()]);
        self.branches.borrow_mut().insert(into.into(), sha.clone());
        Ok(Merged::Commit { sha, tree })
    }

    fn tree(&self, _: &str, sha: &str) -> Result<String, ApiError> {
        Ok(self.trees.borrow()[sha].clone())
    }
}

impl Reverts for Hub {
    fn make_commit(
        &self,
        _: &str,
        tree: &str,
        parent: &str,
        message: &str,
    ) -> Result<String, ApiError> {
        let sha = self.sha();
        self.trees.borrow_mut().insert(sha.clone(), tree.into());
        self.parents
            .borrow_mut()
            .insert(sha.clone(), vec![parent.into()]);
        self.made
            .borrow_mut()
            .push((tree.into(), parent.into(), message.into()));
        Ok(sha)
    }

    fn open_pull(
        &self,
        repo: &str,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<u64, ApiError> {
        let open = self
            .pulls
            .borrow()
            .iter()
            .any(|p| p["state"] == "open" && p["head"]["ref"] == head);
        if open {
            return Err(ApiError::new("A pull request already exists"));
        }
        let number = 100 + self.pulls.borrow().len() as u64;
        let sha = self.branches.borrow()[head].clone();
        let at = self.now();
        self.pulls.borrow_mut().push(json!({
            "number": number, "state": "open", "title": title, "body": body,
            "created_at": at,
            "user": {"login": STEWARD, "type": "Bot"},
            "merged_at": null, "merge_commit_sha": null,
            "base": {"sha": TRUNK, "ref": base, "repo": {"default_branch": "main"}},
            "head": {"sha": sha, "ref": head,
                "repo": {"full_name": if repo.is_empty() { "FieldmouseWorks/remi" } else { repo },
                    "default_branch": "main"}},
        }));
        Ok(number)
    }

    fn pulls_from(&self, _: &str, head: &str) -> Result<Vec<u64>, ApiError> {
        Ok(self
            .pulls
            .borrow()
            .iter()
            .filter(|p| p["head"]["ref"] == head)
            .filter_map(|p| p["number"].as_u64())
            .collect())
    }
}

fn parse<T: serde::de::DeserializeOwned>(v: &Value) -> T {
    serde_json::from_value(v.clone()).unwrap()
}

impl Source for Hub {
    fn pull(&self, _: &str, number: u64) -> Result<PullRequest, ApiError> {
        Ok(parse(&self.pull_json(number)))
    }

    fn pulls(&self, _: &str, _: Pulls) -> Result<Vec<PullRequest>, ApiError> {
        Ok(self.pulls.borrow().iter().map(parse).collect())
    }

    fn check_runs(&self, _: &str, _: &str) -> Result<Vec<CheckRun>, ApiError> {
        Ok(Vec::new())
    }

    fn statuses(&self, _: &str, sha: &str) -> Result<Vec<Status>, ApiError> {
        let settled = self.settled.borrow().iter().any(|s| s == sha);
        Ok(parse(&json!(if settled {
            vec![json!({"context": "review-settled", "state": "success",
                "creator": {"login": STEWARD, "type": "Bot"},
                "created_at": "2026-10-27T11:00:00Z"})]
        } else {
            Vec::new()
        })))
    }

    fn reviews(&self, _: &str, _: u64) -> Result<Vec<Review>, ApiError> {
        Ok(Vec::new())
    }

    fn commit(&self, _: &str, sha: &str) -> Result<Commit, ApiError> {
        let parents = self.parents.borrow().get(sha).cloned().unwrap_or_default();
        Ok(parse(&json!({
            "sha": sha,
            "parents": parents.iter().map(|p| json!({"sha": p})).collect::<Vec<_>>(),
        })))
    }

    fn commit_pulls(&self, _: &str, sha: &str) -> Result<Vec<u64>, ApiError> {
        Ok(self.linked.borrow().get(sha).cloned().unwrap_or_default())
    }

    fn timeline(&self, _: &str, number: u64) -> Result<Vec<Event>, ApiError> {
        let turns = self.turns.borrow();
        Ok(turns
            .get(&number)
            .into_iter()
            .flatten()
            .map(|(at, event)| parse(&json!({"event": event, "created_at": at})))
            .collect())
    }

    fn merge_base(&self, _: &str, a: &str, _: &str) -> Result<String, ApiError> {
        Ok(a.into())
    }
}

fn store(dir: &tempfile::TempDir) -> Store {
    let lock = conary_core::log::Lock::take(dir.path()).unwrap();
    Store::open(lock, SigningKey::from_bytes(&[5; 32]), None).unwrap()
}

/// A change in the ledger, PR 5, landed as AFTER on BEFORE, titled `title`.
fn landed(store: &mut Store, title: Option<&str>) -> ChangeId {
    let change = ChangeId::new().unwrap();
    store
        .record(Body::Observe {
            change,
            repo: repo(),
            pr: 5,
            author: Actor {
                kind: ActorKind::Agent,
                login: "TusanHomichi".into(),
                session: None,
            },
        })
        .unwrap();
    if let Some(title) = title {
        store
            .record(Body::Intent {
                change,
                summary: title.into(),
                body: String::new(),
                task: None,
            })
            .unwrap();
    }
    land(store, change, BEFORE, AFTER);
    change
}

fn land(store: &mut Store, change: ChangeId, before: &str, after: &str) {
    if store.ledger().change(change).unwrap().current.is_none() {
        store
            .record(Body::Revision {
                change,
                base: BEFORE.parse().unwrap(),
                head: "4444444444444444444444444444444444444444".parse().unwrap(),
            })
            .unwrap();
    }
    let revision = store.ledger().change(change).unwrap().current.unwrap();
    store
        .record(Body::Land {
            change,
            revision,
            before: before.parse().unwrap(),
            after: after.parse().unwrap(),
            by: Authority::Status {
                login: STEWARD.into(),
            },
        })
        .unwrap();
}

fn undo_of(store: &mut Store, hub: &Hub, x: ChangeId) -> Result<Undone, UndoError> {
    undo(store, hub, hub, &config(), x)
}

#[test]
fn an_undo_reverts_the_landing_alone_brings_trunk_in_and_is_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new();
    let x = landed(
        &mut store,
        Some("autoland: merge only the head that was checked"),
    );
    let got = undo_of(&mut store, &hub, x).unwrap();
    assert_eq!(got.how, How::Opened { conflicts: false });
    assert_eq!((got.pr, got.reverts.clone()), (100, vec![x]));
    // The revert: trunk's tree from before it landed, on its landing.
    let title = "autoland: undo #5, merge only the head that was checked";
    assert_eq!(
        hub.made.borrow().clone(),
        [(
            "before".to_owned(),
            AFTER.to_owned(),
            format!("{title}\n\nThis reverts {AFTER}, #5's landing.")
        )]
    );
    // Its branch, with trunk merged in, is the PR's head.
    let branch = undo_branch(x, 1);
    assert_eq!(branch, format!("conary/undo/{x}/1"));
    let pull = hub.pull_json(100);
    assert_eq!(pull["head"]["ref"], branch);
    assert_eq!(pull["base"]["ref"], "main");
    assert_eq!(pull["title"], title);
    let body = pull["body"].as_str().unwrap();
    assert!(body.starts_with(&format!(
        "Undoes #5, which landed as {AFTER} (Conary change `{x}`)"
    )));
    assert!(!body.contains("conflicts"));
    let head = hub.branches.borrow()[&branch].clone();
    assert_eq!(hub.trees.borrow()[&head], "trunk without the change");
    // The ledger holds it as the change's undo, asked for by the change.
    let u = store.ledger().change(got.change).unwrap();
    assert_eq!(u.pr, 100);
    let recorded = u.undo.as_ref().unwrap();
    assert_eq!(
        (recorded.reverts.clone(), recorded.asked.clone()),
        (vec![x], Asked::Change)
    );
    assert_eq!(store.ledger().undo_of(x), Some(got.change));
    assert!(got.recorded > 0);
    // Run again, it finds it: nothing new made, opened or recorded.
    let again = undo_of(&mut store, &hub, x).unwrap();
    assert_eq!(
        (again.change, again.pr, again.how, again.recorded),
        (got.change, 100, How::Found, 0)
    );
    assert_eq!((hub.made.borrow().len(), hub.pulls.borrow().len()), (1, 1));
}

#[test]
fn a_revert_that_conflicts_with_trunk_opens_as_it_is_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new();
    hub.merging.replace(None);
    let x = landed(&mut store, Some("tools: a plain sentence"));
    let got = undo_of(&mut store, &hub, x).unwrap();
    assert_eq!(got.how, How::Opened { conflicts: true });
    let pull = hub.pull_json(got.pr);
    // The head is the revert alone, on the landing.
    let revert = hub.branches.borrow()[&undo_branch(x, 1)].clone();
    assert_eq!(pull["head"]["sha"], revert);
    assert_eq!(hub.parent(&revert), AFTER);
    let body = pull["body"].as_str().unwrap();
    assert!(body.contains("this conflicts with main: merge main in"));
}

#[test]
fn a_change_whose_landing_is_trunk_has_its_revert_on_trunk_already() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new();
    hub.branches
        .borrow_mut()
        .insert("main".into(), AFTER.into());
    let x = landed(&mut store, Some("tools: a plain sentence"));
    let got = undo_of(&mut store, &hub, x).unwrap();
    assert_eq!(got.how, How::Opened { conflicts: false });
    let head = hub.pull_json(got.pr)["head"]["sha"].clone();
    assert_eq!(hub.parent(head.as_str().unwrap()), AFTER);
}

#[test]
fn a_revert_that_changes_nothing_opens_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new();
    // Trunk took the change out some other way.
    hub.merging.replace(Some("trunk".into()));
    let x = landed(&mut store, Some("tools: a plain sentence"));
    let err = undo_of(&mut store, &hub, x).unwrap_err();
    assert!(matches!(err, UndoError::Nothing(c) if c == x), "{err}");
    assert!(hub.pulls.borrow().is_empty());
    assert!(hub.branch("", &undo_branch(x, 1)).unwrap().is_none());
    assert_eq!(store.ledger().undo_of(x), None);
}

#[test]
fn a_run_that_stopped_partway_is_taken_up_where_it_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new();
    let x = landed(&mut store, Some("tools: a plain sentence"));
    // Its branch made, and no PR: it's made again, since what a stopped
    // run put on it needn't be what's asked for now.
    let earlier = hub.make_commit("", "before", AFTER, "m").unwrap();
    hub.create_branch("", &undo_branch(x, 1), &earlier).unwrap();
    let got = undo_of(&mut store, &hub, x).unwrap();
    assert_eq!(got.how, How::Opened { conflicts: false });
    let head = hub.pull_json(got.pr)["head"]["sha"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(!hub.holds(&head, &earlier));
    assert_eq!(hub.made.borrow().len(), 2);

    // Its PR opened, and not recorded: it's recorded.
    let dir = tempfile::tempdir().unwrap();
    let mut fresh = self::store(&dir);
    let y = landed(&mut fresh, Some("tools: a plain sentence"));
    let pr = stopped(&hub, y);
    let got = undo_of(&mut fresh, &hub, y).unwrap();
    assert_eq!((got.pr, got.how), (pr, How::Found));
    assert!(got.recorded > 0);
    assert_eq!(fresh.ledger().undo_of(y), Some(got.change));
}

#[test]
fn an_undo_closed_leaves_the_next_a_branch_of_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new();
    let x = landed(&mut store, Some("tools: a plain sentence"));
    let first = undo_of(&mut store, &hub, x).unwrap();
    // Closed on GitHub, and asked again before anything reads it: it's read
    // first, so no closed PR is taken for the open undo.
    hub.close(first.pr);
    let second = undo_of(&mut store, &hub, x).unwrap();
    assert_eq!(
        store.ledger().change(first.change).unwrap().status(),
        Kept::Closed
    );
    assert_ne!(second.change, first.change);
    assert_eq!(second.how, How::Opened { conflicts: false });
    assert_eq!(hub.pull_json(second.pr)["head"]["ref"], undo_branch(x, 2));
}

#[test]
fn an_undo_landed_on_github_is_read_before_another_is_made() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new();
    let x = landed(&mut store, Some("tools: a plain sentence"));
    let first = undo_of(&mut store, &hub, x).unwrap();
    hub.land(first.pr);
    assert!(matches!(
        undo_of(&mut store, &hub, x),
        Err(UndoError::Reverted { change, by }) if change == x && by == first.change
    ));
    assert_eq!(store.ledger().change(x).unwrap().status(), Kept::Reverted);
    assert_eq!(hub.pulls.borrow().len(), 1);
}

/// A run that opened its PR and stopped before recording it: the PR, on
/// `x`'s first undo branch, with its revert.
fn stopped(hub: &Hub, x: ChangeId) -> u64 {
    stopped_at(hub, x, 1)
}

/// The same, on the branch of `x`'s `attempt`th undo.
fn stopped_at(hub: &Hub, x: ChangeId, attempt: usize) -> u64 {
    let revert = hub.make_commit("", "before", AFTER, "m").unwrap();
    hub.create_branch("", &undo_branch(x, attempt), &revert)
        .unwrap();
    let marker = Marker {
        reverts: vec![x],
        asked: Asked::Change,
    };
    hub.open_pull("", &undo_branch(x, attempt), "main", "t", &marker.line())
        .unwrap()
}

#[test]
fn a_stopped_run_s_pr_is_its_undo_whatever_became_of_it() {
    // Landed since, its branch gone: it's what reverted the change.
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new();
    let x = landed(&mut store, Some("tools: a plain sentence"));
    let pr = stopped(&hub, x);
    hub.land(pr);
    let err = undo_of(&mut store, &hub, x).unwrap_err();
    let u = store.ledger().by_pr(&repo(), pr).unwrap().id;
    assert!(
        matches!(err, UndoError::Reverted { change, by } if change == x && by == u),
        "{err}"
    );
    assert_eq!(store.ledger().undo_of(x), Some(u));
    assert_eq!(hub.pulls.borrow().len(), 1);

    // Closed since: the next attempt takes a branch of its own.
    let dir = tempfile::tempdir().unwrap();
    let mut store = self::store(&dir);
    let hub = Hub::new();
    let y = landed(&mut store, Some("tools: a plain sentence"));
    let pr = stopped(&hub, y);
    hub.close(pr);
    let got = undo_of(&mut store, &hub, y).unwrap();
    let closed = store.ledger().by_pr(&repo(), pr).unwrap();
    assert_eq!(closed.status(), Kept::Closed);
    assert_eq!(
        store.ledger().change(y).unwrap().undone_by,
        [closed.id, got.change]
    );
    assert_eq!(hub.pull_json(got.pr)["head"]["ref"], undo_branch(y, 2));

    // Landed, and undone in turn: undoing that is what reverts it again.
    let dir = tempfile::tempdir().unwrap();
    let mut store = self::store(&dir);
    let hub = Hub::new();
    let z = landed(&mut store, Some("tools: a plain sentence"));
    let pr = stopped(&hub, z);
    hub.land(pr);
    let u = ingest::pull(&mut store, &hub, &config(), &repo(), pr)
        .unwrap()
        .change;
    let w = undo_of(&mut store, &hub, u).unwrap();
    // Landed, and nothing has read that since.
    hub.land(w.pr);
    let err = undo_of(&mut store, &hub, z).unwrap_err();
    assert!(
        matches!(err, UndoError::UndoneUndo { change, undo, by }
            if change == z && undo == u && by == w.change),
        "{err}"
    );
    assert_eq!(store.ledger().change(z).unwrap().status(), Kept::Landed);
}

#[test]
fn only_a_landed_change_not_yet_reverted_is_undone() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new();
    let unknown = ChangeId::new().unwrap();
    assert!(matches!(
        undo_of(&mut store, &hub, unknown),
        Err(UndoError::Unknown(c)) if c == unknown
    ));
    let open = ChangeId::new().unwrap();
    store
        .record(Body::Observe {
            change: open,
            repo: repo(),
            pr: 6,
            author: Actor {
                kind: ActorKind::Person,
                login: "TusanHomichi".into(),
                session: None,
            },
        })
        .unwrap();
    assert!(matches!(
        undo_of(&mut store, &hub, open),
        Err(UndoError::NotLanded(c)) if c == open
    ));
    let x = landed(&mut store, Some("tools: a plain sentence"));
    let u = undo_of(&mut store, &hub, x).unwrap();
    land(
        &mut store,
        u.change,
        TRUNK,
        "5555555555555555555555555555555555555555",
    );
    assert_eq!(store.ledger().change(x).unwrap().status(), Kept::Reverted);
    assert!(matches!(
        undo_of(&mut store, &hub, x),
        Err(UndoError::Reverted { change, by }) if change == x && by == u.change
    ));
    assert_eq!(hub.pulls.borrow().len(), 1);
    // Its undo undone puts it back, and only undoing that reverts it again.
    let w = undo_of(&mut store, &hub, u.change).unwrap();
    land(
        &mut store,
        w.change,
        "5555555555555555555555555555555555555555",
        "6666666666666666666666666666666666666666",
    );
    assert_eq!(store.ledger().change(x).unwrap().status(), Kept::Landed);
    assert!(matches!(
        undo_of(&mut store, &hub, x),
        Err(UndoError::UndoneUndo { change, undo, by })
            if change == x && undo == u.change && by == w.change
    ));
    assert_eq!(hub.pulls.borrow().len(), 2);
}

#[test]
fn an_undo_is_titled_in_the_style_of_what_it_reverts() {
    for (said, title) in [
        (
            Some("autoland, train: land in order"),
            "autoland, train: undo #5, land in order",
        ),
        (Some("Fix the thing"), "undo: #5, Fix the thing"),
        (None, "undo: #5"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(&dir);
        let x = landed(&mut store, said);
        assert_eq!(super::title(store.ledger().change(x).unwrap()), title);
    }
}

#[test]
fn an_undo_reopened_on_github_is_found_not_made_again() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new();
    let x = landed(&mut store, Some("tools: a plain sentence"));
    let first = undo_of(&mut store, &hub, x).unwrap();
    let read = |store: &mut Store| {
        ingest::pull(store, &hub, &config(), &repo(), first.pr).unwrap();
    };
    hub.close(first.pr);
    read(&mut store);
    // Reopened since it was read closed.
    hub.reopen(first.pr);
    let got = undo_of(&mut store, &hub, x).unwrap();
    assert_eq!((got.change, got.how), (first.change, How::Found));
    // And landed since, unread.
    hub.close(first.pr);
    read(&mut store);
    hub.reopen(first.pr);
    hub.land(first.pr);
    assert!(matches!(
        undo_of(&mut store, &hub, x),
        Err(UndoError::Reverted { change, by }) if change == x && by == first.change
    ));
    assert_eq!(hub.pulls.borrow().len(), 1);
}

#[test]
fn an_undo_s_own_undo_is_read_down_the_tree() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new();
    let x = landed(&mut store, Some("tools: a plain sentence"));
    let u = undo_of(&mut store, &hub, x).unwrap();
    hub.land(u.pr);
    ingest::pull(&mut store, &hub, &config(), &repo(), u.pr).unwrap();
    // The undo of the undo, from a run that stopped before recording it,
    // landed: nothing has read either.
    let pr = stopped(&hub, u.change);
    hub.land(pr);
    let err = undo_of(&mut store, &hub, x).unwrap_err();
    let w = store.ledger().by_pr(&repo(), pr).unwrap().id;
    assert!(
        matches!(err, UndoError::UndoneUndo { change, undo, by }
            if change == x && undo == u.change && by == w),
        "{err}"
    );
    assert_eq!(store.ledger().change(x).unwrap().status(), Kept::Landed);
}

#[test]
fn a_stopped_run_closed_is_recorded_beside_the_undo_that_reopened() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new();
    let x = landed(&mut store, Some("tools: a plain sentence"));
    let first = undo_of(&mut store, &hub, x).unwrap();
    hub.close(first.pr);
    ingest::pull(&mut store, &hub, &config(), &repo(), first.pr).unwrap();
    // The next attempt's run stopped before recording its PR, which closed,
    // and then the first reopened.
    let second = stopped_at(&hub, x, 2);
    hub.close(second);
    hub.reopen(first.pr);
    let got = undo_of(&mut store, &hub, x).unwrap();
    assert_eq!((got.change, got.how), (first.change, How::Found));
    let closed = store.ledger().by_pr(&repo(), second).unwrap();
    assert_eq!(closed.status(), Kept::Closed);
    assert_eq!(
        store.ledger().change(x).unwrap().undone_by,
        [first.change, closed.id]
    );
    // Recorded once: run again, there's nothing new.
    let again = undo_of(&mut store, &hub, x).unwrap();
    assert_eq!((again.change, again.recorded), (first.change, 0));
    assert_eq!(hub.pulls.borrow().len(), 2);
}

#[test]
fn a_stopped_run_that_merged_after_its_rival_landed_is_trunk_moving_outside() {
    // The next attempt's run stopped before recording its PR, which closed;
    // the first reopened and landed, then the second reopened and merged
    // too. Taken up by `undo` itself, or after a sync read it first, or
    // after the second alone was read: the first is the undo, and the
    // second's merge reverted it again outside Conary.
    for read_first in [None, Some(true), Some(false)] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(&dir);
        let hub = Hub::new();
        let x = landed(&mut store, Some("tools: a plain sentence"));
        let first = undo_of(&mut store, &hub, x).unwrap();
        hub.close(first.pr);
        ingest::pull(&mut store, &hub, &config(), &repo(), first.pr).unwrap();
        let second = stopped_at(&hub, x, 2);
        hub.close(second);
        hub.reopen(first.pr);
        hub.land(first.pr);
        hub.reopen(second);
        let again = hub.land(second);
        match read_first {
            Some(true) => _ = ingest::sync(&mut store, &hub, &config(), &repo()).unwrap(),
            Some(false) => _ = ingest::pull(&mut store, &hub, &config(), &repo(), second).unwrap(),
            None => {}
        }
        let err = undo_of(&mut store, &hub, x).unwrap_err();
        assert!(
            matches!(err, UndoError::Reverted { change, by } if change == x && by == first.change),
            "{read_first:?}: {err}"
        );
        let held = store.ledger().by_pr(&repo(), second).unwrap();
        assert_eq!(held.status(), Kept::Closed, "{read_first:?}");
        assert_eq!(
            store.ledger().change(x).unwrap().undone_by,
            [first.change, held.id]
        );
        let outside = store.ledger().outside();
        assert_eq!(outside.len(), 1, "{read_first:?}");
        assert_eq!(outside[0].after.as_str(), again);
        // Taken up again, nothing's new.
        let err = undo_of(&mut store, &hub, x).unwrap_err();
        assert!(matches!(err, UndoError::Reverted { .. }), "{err}");
    }
}

mod group;
