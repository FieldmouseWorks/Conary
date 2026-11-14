//! A simulated GitHub, in memory, that the train, undo, class and ingest
//! are tested against, and the landing flow will be (M6, Conary#256): one
//! model of repos, commits, branches, PRs, statuses, check runs and their
//! attestations, behind every trait the code talks to GitHub through.
//!
//! **Commits hold patches.** A commit is its parents and the set of
//! patches its tree holds, and a tree is named by that set alone, so two
//! commits holding the same patches have one tree whatever order they came
//! in, as git's would. A merge is three-way: what either side added to or
//! took from their merge base, together, unless a patch one side changed
//! clashes with a different one the other side changed ([`Hub::clash`]),
//! which conflicts. A revert is a commit of an earlier tree, so merging
//! trunk into it brings what came after and leaves out what it reverts, and
//! a revert of what trunk already took out changes nothing. A PR's head is
//! a commit of its own patch on the branch it forked from; landing one
//! squashes what its head changed since that fork onto its base branch.
//!
//! **GitHub's answers are its own shapes.** What the model makes (a PR, a
//! commit, a status, a check run, a timeline event, a comparison) is built
//! from a response recorded from GitHub's REST API (`sim/fixtures/`), with
//! the model's fields written over it, and parsed by the same types
//! [`crate::api::Api`] parses GitHub's with. An answer a test writes itself
//! (`put_*`) is served as written, so a field it leaves out is one GitHub
//! didn't send.
//!
//! **GitHub misbehaves on cue** ([`faults`]): a call that fails before it
//! does anything, a write whose answer is lost after it's done, a read that
//! answers from a moment ago (GitHub's lag, which is all a delay can show
//! code that waits for nothing), and a race, something else changing
//! GitHub just before a call. Every call is logged with what it asked, so a
//! test can count what was read.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::{Value, json};
use sha1::{Digest, Sha1};

use conary_core::policy::Touched;

use crate::attest::testing;
use crate::train::Mark;

mod ci;
mod faults;
mod fixtures;
mod flow;
mod flow_write;
mod forge;

pub(crate) use faults::{Call, On};

/// The steward's App, whose `review-settled` statuses settle a head.
pub(crate) const STEWARD: &str = "fieldmouseworks-steward[bot]";
/// The repos the tests are about.
pub(crate) const REMI: &str = "FieldmouseWorks/remi";
/// What the runners a job gets to itself are named, then its job's id.
pub(crate) const FRESH: &str = "fresh-remi-";
/// What a patch touches when a test hasn't said.
const SOME_FILE: &str = "src/lib.rs";
/// The ref every PR's head is on, unless a test opens it from a branch.
const HEAD_REF: &str = "claude/x";

/// The simulated GitHub. Every read and write goes through its traits'
/// impls ([`forge`]); tests set it up and look inside through the methods
/// here.
pub(crate) struct Hub {
    world: RefCell<World>,
    faults: faults::Faults,
    /// Every call asked of it, with its arguments, in order.
    log: RefCell<Vec<(Call, Vec<String>)>>,
    /// The tick a clock reads: each thing that happens takes one.
    clock: Cell<u64>,
}

/// Everything GitHub holds. Cloned whole for a read that lags.
#[derive(Clone, Default)]
pub(crate) struct World {
    repos: BTreeMap<String, Repo>,
    commits: HashMap<String, Commit>,
    /// Every tree a commit has had, by its name: the patches it holds.
    trees: HashMap<String, BTreeSet<String>>,
    /// Commit statuses by commit, newest first, as GitHub lists them.
    statuses: HashMap<String, Vec<Value>>,
    /// Check runs by commit, as GitHub lists them.
    checks: HashMap<String, Vec<Value>>,
    annotations: HashMap<u64, Vec<String>>,
    pushes: HashMap<(String, String), BTreeMap<String, String>>,
    history_limits: HashMap<(String, String), usize>,
    commit_records: HashMap<String, Value>,
    diffs: HashMap<(String, String), String>,
    /// Each check run's job's artifacts, by the job's check run id: name
    /// and GitHub's zip of it.
    artifacts: HashMap<u64, Vec<(String, Vec<u8>)>>,
    /// The jobs, by check run id, whose artifacts GitHub lists as expired.
    expired: BTreeSet<u64>,
    /// The runner each job ran on, when it isn't a fresh one of its own.
    runners: HashMap<u64, String>,
    /// Pairs of patches that change the same lines.
    clashes: BTreeSet<(String, String)>,
    /// Patches that change lines every other patch does.
    clashing: BTreeSet<String>,
    /// The files each patch changes, when it isn't [`SOME_FILE`] alone.
    touched: HashMap<String, Vec<Touched>>,
    /// Every merge made, as (repo, branch, head).
    merges: Vec<(String, String, String)>,
    /// Commits made, as (tree, parent, message), by `make_commit`.
    made: Vec<(String, String, String)>,
    /// PRs whose merge GitHub refuses, and what it says.
    refused: HashMap<(String, u64), String>,
    /// The last commit sha, check run id and PR number given out.
    shas: u64,
    runs: u64,
}

#[derive(Clone, Default)]
struct Repo {
    default: String,
    branches: BTreeMap<String, String>,
    /// PRs as GitHub's REST API shows them, by number.
    pulls: BTreeMap<u64, Value>,
    reviews: HashMap<u64, Vec<Value>>,
    comments: HashMap<u64, Vec<Value>>,
    flow: HashMap<u64, crate::flow::Snapshot>,
    /// Each PR's timeline events, oldest first.
    timeline: HashMap<u64, Vec<Value>>,
    /// The PRs each commit is linked to: a PR's merge links its commit.
    linked: HashMap<String, Vec<u64>>,
}

#[derive(Clone, Debug)]
struct Commit {
    parents: Vec<String>,
    patches: BTreeSet<String>,
    /// Made and signed by GitHub, as its merges are.
    by_github: bool,
}

/// A status the train posted, read back.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Posted {
    pub(crate) sha: String,
    pub(crate) mark: Mark,
    pub(crate) description: String,
    pub(crate) url: Option<String>,
}

/// The name of the tree holding `patches`: forty hex digits, as git's are.
pub(crate) fn tree_of<S: AsRef<str>>(patches: &[S]) -> String {
    let set: BTreeSet<&str> = patches.iter().map(AsRef::as_ref).collect();
    let mut h = Sha1::new();
    for p in set {
        h.update(p.as_bytes());
        h.update(b"\n");
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn names(set: &BTreeSet<String>) -> Vec<String> {
    set.iter().cloned().collect()
}

/// Parses one of GitHub's answers as the API client does.
pub(crate) fn parse<T: serde::de::DeserializeOwned>(v: &Value) -> T {
    serde_json::from_value(v.clone()).unwrap_or_else(|e| panic!("{e}: {v}"))
}

impl Hub {
    /// GitHub with `repo` alone, its default branch `main` at `trunk`, a
    /// commit holding nothing; and the signer's repo, its default branch
    /// at the signer commit.
    pub(crate) fn new(repo: &str, trunk: &str) -> Self {
        let hub = Self {
            world: RefCell::default(),
            faults: faults::Faults::default(),
            log: RefCell::default(),
            clock: Cell::new(0),
        };
        hub.add_repo(repo, "main", trunk);
        let signer = testing::signer().repo.to_string();
        hub.add_repo(&signer, "main", "5150000000000000000000000000000000000000");
        hub.set_signer(&[testing::SIGNER_SHA]);
        hub
    }

    /// Another repo, its default branch at `root`, a commit holding nothing.
    pub(crate) fn add_repo(&self, repo: &str, default: &str, root: &str) {
        self.commit_as(root, &[], BTreeSet::new());
        let mut w = self.world.borrow_mut();
        let r = w.repos.entry(repo.into()).or_default();
        r.default = default.into();
        r.branches.insert(default.into(), root.into());
    }

    /// The time now, in GitHub's format, a second later than last asked.
    pub(crate) fn now(&self) -> String {
        let t = self.clock.get() + 1;
        self.clock.set(t);
        format!("2026-10-29T12:{:02}:{:02}Z", t / 60 % 60, t % 60)
    }

    fn next_sha(&self) -> String {
        let mut w = self.world.borrow_mut();
        w.shas += 1;
        format!("{:0>40x}", 0x00c0_ffee_0000_u64 + w.shas)
    }

    // Commits.

    /// Makes commit `sha` on `parents`, holding `patches`; its tree is
    /// named for them.
    fn commit_as(&self, sha: &str, parents: &[String], patches: BTreeSet<String>) {
        let mut w = self.world.borrow_mut();
        w.trees.insert(tree_of(&names(&patches)), patches.clone());
        w.commits.insert(
            sha.into(),
            Commit {
                parents: parents.to_vec(),
                patches,
                by_github: false,
            },
        );
    }

    /// Commit `sha` on `parents`, holding what its first parent does and a
    /// patch of its own: made and signed by GitHub, as its merges are, or
    /// by someone else.
    pub(crate) fn put_commit(&self, sha: &str, parents: &[&str], by_github: bool) {
        let first = parents
            .first()
            .and_then(|p| self.world.borrow().commits.get(*p).cloned());
        let mut patches = first.map(|c| c.patches).unwrap_or_default();
        patches.insert(sha.into());
        let parents: Vec<String> = parents.iter().map(|p| (*p).to_owned()).collect();
        self.commit_as(sha, &parents, patches);
        if by_github {
            self.by_github(sha);
        }
    }

    /// Commit `sha` on `on`, adding the patch `patch` to what it holds.
    pub(crate) fn change(&self, sha: &str, on: &str, patch: &str) -> String {
        let mut patches = self.patches(on);
        patches.insert(patch.into());
        self.commit_as(sha, &[on.into()], patches);
        sha.into()
    }

    /// A new commit on `on` adding `patch`: its sha.
    pub(crate) fn add(&self, on: &str, patch: &str) -> String {
        let sha = self.next_sha();
        self.change(&sha, on, patch)
    }

    /// A new commit on `on` taking the patch `patch` out of what it holds,
    /// as a revert made outside the code under test does: its sha.
    pub(crate) fn take_out(&self, on: &str, patch: &str) -> String {
        let sha = self.next_sha();
        self.take_out_as(&sha, on, patch)
    }

    /// The same, as commit `sha`.
    pub(crate) fn take_out_as(&self, sha: &str, on: &str, patch: &str) -> String {
        let mut patches = self.patches(on);
        patches.remove(patch);
        self.commit_as(sha, &[on.into()], patches);
        sha.into()
    }

    /// Commit `sha` was made and signed by GitHub, as its merges are.
    pub(crate) fn by_github(&self, sha: &str) {
        self.world
            .borrow_mut()
            .commits
            .get_mut(sha)
            .unwrap()
            .by_github = true;
    }

    /// The patches commit `sha` holds.
    pub(crate) fn patches(&self, sha: &str) -> BTreeSet<String> {
        self.world.borrow().commits[sha].patches.clone()
    }

    /// The patches commit `sha` holds, as a list.
    pub(crate) fn holding(&self, sha: &str) -> Vec<String> {
        names(&self.patches(sha))
    }

    /// The tree of commit `sha`.
    pub(crate) fn tree_at(&self, sha: &str) -> String {
        tree_of(&self.holding(sha))
    }

    pub(crate) fn parents(&self, sha: &str) -> Vec<String> {
        self.world.borrow().commits[sha].parents.clone()
    }

    /// Commit `sha`'s first parent.
    pub(crate) fn parent(&self, sha: &str) -> String {
        self.parents(sha)[0].clone()
    }

    /// Whether `commit` is `at` or one of its ancestors.
    pub(crate) fn reaches(&self, at: &str, commit: &str) -> bool {
        self.world.borrow().reaches(at, commit)
    }

    /// Patches `a` and `b` change the same lines.
    pub(crate) fn clash(&self, a: &str, b: &str) {
        let pair = if a < b { (a, b) } else { (b, a) };
        self.world
            .borrow_mut()
            .clashes
            .insert((pair.0.into(), pair.1.into()));
    }

    /// No patch clashes with another any more.
    pub(crate) fn unclash(&self) {
        let mut w = self.world.borrow_mut();
        w.clashes.clear();
        w.clashing.clear();
    }

    /// Patch `a` changes lines every other patch does.
    pub(crate) fn clash_with_all(&self, a: &str) {
        self.world.borrow_mut().clashing.insert(a.into());
    }

    /// Patch `patch` changes `paths`.
    pub(crate) fn touch(&self, patch: &str, paths: &[&str]) {
        let files = paths
            .iter()
            .map(|p| Touched {
                path: (*p).into(),
                from: None,
                new: false,
            })
            .collect();
        self.world.borrow_mut().touched.insert(patch.into(), files);
    }

    // Branches.

    pub(crate) fn default_of(&self, repo: &str) -> String {
        self.world.borrow().repos[repo].default.clone()
    }

    /// The repo's default branch is `branch` now, as every PR shows.
    pub(crate) fn set_default(&self, repo: &str, branch: &str) {
        self.repo_mut(repo, |r| {
            r.default = branch.into();
            for p in r.pulls.values_mut() {
                p["base"]["repo"]["default_branch"] = json!(branch);
            }
        });
    }

    /// Renames branch `from` to `to`, as GitHub does: the PRs onto it are
    /// onto `to` now, and a default branch renamed is still the default.
    pub(crate) fn rename_branch(&self, repo: &str, from: &str, to: &str) {
        let sha = self.remove_branch(repo, from).unwrap();
        self.set_branch(repo, to, &sha);
        if self.default_of(repo) == from {
            self.set_default(repo, to);
        }
        self.repo_mut(repo, |r| {
            for p in r.pulls.values_mut().filter(|p| p["base"]["ref"] == from) {
                p["base"]["ref"] = json!(to);
            }
        });
    }

    pub(crate) fn branch_at(&self, repo: &str, name: &str) -> Option<String> {
        self.world.borrow().repos[repo].branches.get(name).cloned()
    }

    /// Points branch `name` at `sha`, made or moved.
    pub(crate) fn set_branch(&self, repo: &str, name: &str, sha: &str) {
        self.world
            .borrow_mut()
            .repos
            .get_mut(repo)
            .unwrap()
            .branches
            .insert(name.into(), sha.into());
    }

    pub(crate) fn remove_branch(&self, repo: &str, name: &str) -> Option<String> {
        self.world
            .borrow_mut()
            .repos
            .get_mut(repo)
            .unwrap()
            .branches
            .remove(name)
    }

    /// Every branch's name.
    pub(crate) fn names(&self, repo: &str) -> Vec<String> {
        self.world.borrow().repos[repo]
            .branches
            .keys()
            .cloned()
            .collect()
    }

    /// The patches branch `name` holds.
    pub(crate) fn held(&self, repo: &str, name: &str) -> Vec<String> {
        self.holding(&self.branch_at(repo, name).unwrap())
    }

    /// Every merge made in `repo`, as (branch, head).
    pub(crate) fn merges(&self, repo: &str) -> Vec<(String, String)> {
        self.world
            .borrow()
            .merges
            .iter()
            .filter(|m| m.0 == repo)
            .map(|m| (m.1.clone(), m.2.clone()))
            .collect()
    }

    /// Commits `make_commit` made, as (tree, parent, message).
    pub(crate) fn made(&self) -> Vec<(String, String, String)> {
        self.world.borrow().made.clone()
    }

    /// The signer's repo's default branch: these commits, oldest first, on
    /// its root.
    pub(crate) fn set_signer(&self, chain: &[&str]) {
        let signer = testing::signer().repo.to_string();
        let mut at = self.world.borrow().repos[&signer].branches["main"].clone();
        // Back to its root, then the chain on it.
        while let Some(p) = self.parents(&at).first().cloned() {
            at = p;
        }
        for c in chain {
            self.commit_as(c, &[at.clone()], BTreeSet::new());
            at = (*c).into();
        }
        self.set_branch(&signer, "main", &at);
    }

    // PRs.

    /// Opens PR `number` in `repo` onto its default branch, at a head of
    /// its own patch (named for the head) on the branch's tip, unless
    /// `head` is a commit already: settled by the steward there.
    pub(crate) fn pr(&self, repo: &str, number: u64, head: &str) {
        let base = self.default_of(repo);
        let tip = self.branch_at(repo, &base).unwrap();
        if !self.world.borrow().commits.contains_key(head) {
            self.change(head, &tip, head);
        }
        let mut p = fixtures::pull();
        p["number"] = json!(number);
        p["state"] = json!("open");
        p["title"] = json!("remi: a change");
        p["body"] = json!("");
        p["user"] = fixtures::user("TusanHomichi", "User");
        p["created_at"] = json!(self.now());
        p["merged"] = json!(false);
        p["merged_at"] = Value::Null;
        p["merge_commit_sha"] = Value::Null;
        p["base"]["ref"] = json!(base);
        p["base"]["sha"] = json!(tip);
        p["base"]["repo"]["full_name"] = json!(repo);
        p["base"]["repo"]["default_branch"] = json!(base);
        p["head"]["ref"] = json!(HEAD_REF);
        p["head"]["sha"] = json!(head);
        p["head"]["repo"]["full_name"] = json!(repo);
        p["labels"] = json!([]);
        self.repo_mut(repo, |r| {
            r.pulls.insert(number, p);
        });
        self.settle(head);
    }

    fn repo_mut<T>(&self, repo: &str, f: impl FnOnce(&mut Repo) -> T) -> T {
        f(self.world.borrow_mut().repos.get_mut(repo).unwrap())
    }

    /// PR `number` as GitHub's REST API shows it.
    pub(crate) fn pull_json(&self, repo: &str, number: u64) -> Value {
        self.world.borrow().repos[repo].pulls[&number].clone()
    }

    /// Every PR in `repo`, by number.
    pub(crate) fn pull_numbers(&self, repo: &str) -> Vec<u64> {
        self.world.borrow().repos[repo]
            .pulls
            .keys()
            .copied()
            .collect()
    }

    /// Changes what GitHub shows of PR `number`.
    pub(crate) fn edit_pull(&self, repo: &str, number: u64, f: impl FnOnce(&mut Value)) {
        self.repo_mut(repo, |r| f(r.pulls.get_mut(&number).unwrap()));
    }

    /// GitHub lists exactly `prs` in `repo`, as a test writes its answers.
    pub(crate) fn put_pulls(&self, repo: &str, prs: impl IntoIterator<Item = Value>) {
        self.repo_mut(repo, |r| r.pulls.clear());
        for pr in prs {
            self.put_pull(repo, pr);
        }
    }

    /// Puts PR `pr` in `repo`, as a test writes GitHub's answer.
    pub(crate) fn put_pull(&self, repo: &str, pr: Value) {
        let number = pr["number"].as_u64().unwrap();
        self.repo_mut(repo, |r| {
            r.pulls.insert(number, pr);
        });
    }

    /// The head PR `number` is at.
    pub(crate) fn head_of(&self, repo: &str, number: u64) -> String {
        self.pull_json(repo, number)["head"]["sha"]
            .as_str()
            .unwrap()
            .into()
    }

    /// Pushes `sha` to PR `number`, in place of its head: a commit of its
    /// own patch where the old head forked, unless it's a commit already.
    /// Its old head's statuses stay with the old head.
    pub(crate) fn push(&self, repo: &str, number: u64, sha: &str) {
        let old = self.head_of(repo, number);
        if !self.world.borrow().commits.contains_key(sha) {
            let fork = self.parent(&old);
            self.change(sha, &fork, sha);
        }
        self.edit_pull(repo, number, |p| p["head"]["sha"] = json!(sha));
    }

    /// PR `number` now targets `base`.
    pub(crate) fn retarget(&self, repo: &str, number: u64, base: &str) {
        self.edit_pull(repo, number, |p| p["base"]["ref"] = json!(base));
    }

    /// PR `number`'s labels are `labels`.
    pub(crate) fn label(&self, repo: &str, number: u64, labels: &[&str]) {
        let labels: Vec<Value> = labels.iter().map(|l| json!({"name": l})).collect();
        self.edit_pull(repo, number, |p| p["labels"] = json!(labels));
    }

    /// Head `head` does just what `other`'s does: it holds `other`'s patch
    /// in place of its own.
    pub(crate) fn does_as(&self, head: &str, other: &str) {
        let mut w = self.world.borrow_mut();
        let c = w.commits.get_mut(head).unwrap();
        c.patches.remove(head);
        c.patches.insert(other.into());
        let patches = c.patches.clone();
        w.trees.insert(tree_of(&names(&patches)), patches);
    }

    /// Head `head` is made on `under`'s branch: merging or landing it
    /// brings `under` too.
    pub(crate) fn stack(&self, head: &str, under: &str) {
        let mut patches = self.patches(under);
        patches.insert(head.into());
        self.commit_as(head, &[under.into()], patches);
    }

    /// PR `number`'s merge commit, once it's merged.
    pub(crate) fn merged(&self, repo: &str, number: u64) -> Option<String> {
        self.pull_json(repo, number)["merge_commit_sha"]
            .as_str()
            .map(str::to_owned)
    }

    /// The PRs merged in `repo`.
    pub(crate) fn landed(&self, repo: &str) -> Vec<u64> {
        self.pull_numbers(repo)
            .into_iter()
            .filter(|n| self.merged(repo, *n).is_some())
            .collect()
    }

    /// GitHub takes every PR's merge again.
    pub(crate) fn unrefuse(&self) {
        self.world.borrow_mut().refused.clear();
    }

    /// GitHub refuses PR `number`'s merge, saying `why`.
    pub(crate) fn refuse(&self, repo: &str, number: u64, why: &str) {
        self.world
            .borrow_mut()
            .refused
            .insert((repo.into(), number), why.into());
    }

    /// PR `number` closes or reopens: `state` is `closed` or `open`.
    pub(crate) fn turn(&self, repo: &str, number: u64, state: &str) {
        let event = if state == "open" {
            "reopened"
        } else {
            "closed"
        };
        self.edit_pull(repo, number, |p| p["state"] = json!(state));
        self.happened(repo, number, event);
    }

    pub(crate) fn close(&self, repo: &str, number: u64) {
        self.turn(repo, number, "closed");
    }

    pub(crate) fn reopen(&self, repo: &str, number: u64) {
        self.turn(repo, number, "open");
    }

    /// Something happens on PR `number`'s timeline now.
    pub(crate) fn happened(&self, repo: &str, number: u64, event: &str) {
        let mut e = fixtures::event();
        e["event"] = json!(event);
        e["created_at"] = json!(self.now());
        self.repo_mut(repo, |r| r.timeline.entry(number).or_default().push(e));
    }

    /// PR `number`'s timeline is these events, as (time, event), oldest
    /// first.
    pub(crate) fn put_timeline(&self, repo: &str, number: u64, events: &[(&str, &str)]) {
        self.repo_mut(repo, |r| r.timeline.remove(&number));
        for (at, event) in events {
            self.put_event(repo, number, event, at);
        }
    }

    /// Puts an event GitHub's API sent on PR `number`'s timeline.
    pub(crate) fn put_event(&self, repo: &str, number: u64, event: &str, at: &str) {
        let mut e = fixtures::event();
        e["event"] = json!(event);
        e["created_at"] = json!(at);
        self.repo_mut(repo, |r| r.timeline.entry(number).or_default().push(e));
    }

    /// Lands PR `number` as the steward does outside the train: settled at
    /// its head, then merged with the message the train would give it.
    /// Trunk's new tip.
    pub(crate) fn steward_lands(&self, repo: &str, number: u64) -> String {
        let head = self.head_of(repo, number);
        self.settle(&head);
        let p = self.pull_json(repo, number);
        let tip = self
            .branch_at(repo, p["base"]["ref"].as_str().unwrap())
            .unwrap();
        let messages: Vec<String> = {
            let w = self.world.borrow();
            let excluded: BTreeSet<_> = w.ancestors(&tip).into_iter().collect();
            let mut commits: Vec<String> = w
                .ancestors(&head)
                .into_iter()
                .filter(|c| !excluded.contains(c))
                .collect();
            commits.reverse();
            commits
                .iter()
                .map(|c| {
                    w.commit_record(repo, c)
                        .ok()
                        .and_then(|c| c.commit)
                        .and_then(|c| c.message)
                        .unwrap_or_default()
                })
                .collect()
        };
        let message = crate::train::Message::new(
            p["title"].as_str().unwrap(),
            number,
            messages.iter().map(String::as_str),
        );
        match self.squash(repo, number, &head, &message) {
            Ok(sha) => sha,
            Err(why) => panic!("#{number} won't land: {why}"),
        }
    }

    /// Links commit `sha` to PRs `prs`, as GitHub does a merge's.
    pub(crate) fn link(&self, repo: &str, sha: &str, prs: &[u64]) {
        self.repo_mut(repo, |r| {
            r.linked.insert(sha.into(), prs.to_vec());
        });
    }

    /// Reviews of PR `number`, as a test writes GitHub's answer.
    pub(crate) fn put_reviews(&self, repo: &str, number: u64, reviews: Value) {
        let reviews = reviews.as_array().unwrap().clone();
        self.repo_mut(repo, |r| {
            r.reviews.insert(number, reviews);
        });
    }

    // The log.

    /// What each call of this kind asked, in order.
    pub(crate) fn asked(&self, call: Call) -> Vec<Vec<String>> {
        self.log
            .borrow()
            .iter()
            .filter(|(c, _)| *c == call)
            .map(|(_, a)| a.clone())
            .collect()
    }

    /// How many calls of this kind were made.
    pub(crate) fn count(&self, call: Call) -> usize {
        self.asked(call).len()
    }

    // The model's own moves, which the traits' impls make.

    /// Merges `head` into branch `into` in `repo`, three-way, as GitHub's
    /// merge API does: `None` when it's merged already, an error when it
    /// conflicts.
    fn merge_into(&self, repo: &str, into: &str, head: &str) -> Result<Option<String>, ()> {
        let at = self.branch_at(repo, into).ok_or(())?;
        if self.reaches(&at, head) {
            return Ok(None);
        }
        let merged = self.world.borrow().three_way(&at, head).ok_or(())?;
        let sha = self.next_sha();
        self.commit_as(&sha, &[at, head.into()], merged);
        self.by_github(&sha);
        self.set_branch(repo, into, &sha);
        self.world
            .borrow_mut()
            .merges
            .push((repo.into(), into.into(), head.into()));
        Ok(Some(sha))
    }

    /// Squashes PR `number`'s head `head` onto its base branch, as its
    /// merge does: the new commit, linked to it, and the PR merged and
    /// closed, its branch deleted if it's the repo's own.
    /// Squashes PR `number` at `head` onto its base, as a commit saying
    /// `message`, the way GitHub's merge does.
    fn squash(
        &self,
        repo: &str,
        number: u64,
        head: &str,
        message: &crate::train::Message,
    ) -> Result<String, String> {
        let p = self.pull_json(repo, number);
        let base = p["base"]["ref"].as_str().unwrap().to_owned();
        let tip = self.branch_at(repo, &base).ok_or("no base branch")?;
        let merged = self
            .world
            .borrow()
            .three_way(&tip, head)
            .ok_or("Pull Request is not mergeable")?;
        let sha = self.next_sha();
        self.commit_as(&sha, &[tip], merged);
        self.by_github(&sha);
        let mut record = fixtures::commit();
        record["commit"]["message"] = json!(message.whole());
        self.commit_response(&sha, record);
        self.set_branch(repo, &base, &sha);
        self.link(repo, &sha, &[number]);
        let at = self.now();
        self.edit_pull(repo, number, |p| {
            p["state"] = json!("closed");
            p["merged"] = json!(true);
            p["merged_at"] = json!(at);
            p["merge_commit_sha"] = json!(sha);
        });
        self.happened(repo, number, "merged");
        self.happened(repo, number, "closed");
        let branch = p["head"]["ref"].as_str().unwrap();
        if self.branch_at(repo, branch).as_deref() == Some(head) {
            self.remove_branch(repo, branch);
        }
        Ok(sha)
    }
}

impl World {
    fn reaches(&self, at: &str, commit: &str) -> bool {
        let mut todo = vec![at.to_owned()];
        let mut seen = BTreeSet::new();
        while let Some(c) = todo.pop() {
            if c == commit {
                return true;
            }
            if seen.insert(c.clone()) {
                todo.extend(
                    self.commits
                        .get(&c)
                        .into_iter()
                        .flat_map(|c| c.parents.clone()),
                );
            }
        }
        false
    }

    /// Every ancestor of `at`, itself included, nearest first.
    fn ancestors(&self, at: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut todo = std::collections::VecDeque::from([at.to_owned()]);
        let mut seen = BTreeSet::new();
        while let Some(c) = todo.pop_front() {
            if seen.insert(c.clone()) {
                todo.extend(
                    self.commits
                        .get(&c)
                        .into_iter()
                        .flat_map(|c| c.parents.clone()),
                );
                out.push(c);
            }
        }
        out
    }

    /// `a` and `b`'s merge base, as git picks it: a common ancestor no
    /// other common ancestor descends from. Where there are several, the
    /// least by name, so the answer never hangs on the order of the
    /// arguments or of a merge's parents.
    fn merge_base(&self, a: &str, b: &str) -> Option<String> {
        let of_a: BTreeSet<String> = self.ancestors(a).into_iter().collect();
        let common: BTreeSet<String> = self
            .ancestors(b)
            .into_iter()
            .filter(|c| of_a.contains(c))
            .collect();
        common
            .iter()
            .find(|c| !common.iter().any(|d| d != *c && self.reaches(d, c)))
            .cloned()
    }

    fn clashes(&self, a: &str, b: &str) -> bool {
        let pair = if a < b { (a, b) } else { (b, a) };
        a != b
            && (self.clashing.contains(a)
                || self.clashing.contains(b)
                || self.clashes.contains(&(pair.0.into(), pair.1.into())))
    }

    /// What merging `theirs` into `ours` holds: what either changed since
    /// their merge base, together. `None` when a patch one changed clashes
    /// with another the other changed.
    fn three_way(&self, ours: &str, theirs: &str) -> Option<BTreeSet<String>> {
        let base = self.merge_base(ours, theirs)?;
        let (base, ours, theirs) = (
            &self.commits[&base].patches,
            &self.commits[ours].patches,
            &self.commits[theirs].patches,
        );
        let ours_changed: Vec<&String> = base.symmetric_difference(ours).collect();
        let theirs_changed: Vec<&String> = base.symmetric_difference(theirs).collect();
        for p in &ours_changed {
            if theirs_changed.iter().any(|q| self.clashes(p, q)) {
                return None;
            }
        }
        Some(
            ours.union(theirs)
                .filter(|x| (ours.contains(*x) && theirs.contains(*x)) || !base.contains(*x))
                .cloned()
                .collect(),
        )
    }

    /// The files comparing `base...head` lists: what each patch changes
    /// that `head` added or took out since their merge base.
    fn compare(&self, base: &str, head: &str) -> Option<Vec<Touched>> {
        let fork = self.merge_base(base, head)?;
        let had = &self.commits[&fork].patches;
        let mut files: Vec<Touched> = Vec::new();
        for p in self.commits[head].patches.symmetric_difference(had) {
            let touched = self.touched.get(p).cloned().unwrap_or_else(|| {
                vec![Touched {
                    path: SOME_FILE.into(),
                    from: None,
                    new: false,
                }]
            });
            for t in touched {
                if !files.iter().any(|f| f.path == t.path) {
                    files.push(t);
                }
            }
        }
        Some(files)
    }
}

/// One of GitHub's recorded answers with `over`'s fields written over it.
fn built(mut recorded: Value, over: &Value) -> Value {
    merge_json(&mut recorded, over);
    recorded
}

/// Writes `over`'s fields over `v`'s, objects field by field.
fn merge_json(v: &mut Value, over: &Value) {
    match (v, over) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, x) in b {
                match a.get_mut(k) {
                    Some(y) if y.is_object() && x.is_object() => merge_json(y, x),
                    _ => _ = a.insert(k.clone(), x.clone()),
                }
            }
        }
        (v, over) => *v = over.clone(),
    }
}

mod tests;
