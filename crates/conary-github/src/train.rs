//! The train on GitHub (`docs/DESIGN.md` section 4.7). The ledger says what
//! a repo's train does next ([`Ledger::next`]); this does it on GitHub,
//! behind [`Forge`], and records each step. A candidate is built with the
//! merges API on its build branch, so nothing on the box runs git, and the
//! gate runs on its train branch, made at the finished commit. Green is the
//! train branch's CI, read as the steward reads a PR's, until Conary counts
//! attested evidence itself; a green candidate's changes land through their
//! PRs' merges, in order, with the train's status ([`CONTEXT`]) on each head.
//!
//! [`Ledger::next`]: conary_core::Ledger::next

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use conary_core::ledger::{Candidate, Held, Invalid, State};
use conary_core::op::{Body, CheckKind, Left, Member, Repo, Verdict};
use conary_core::store::StoreError;
use conary_core::{CandidateId, ChangeId, CommitId, Store, TreeId};

use crate::api::{ApiError, CheckRun, Source};
use crate::ingest::{self, Config, IngestError, run_result, status_result};

mod land;
use land::{land_next, open_landing, stopped_landing};

/// Every candidate's branches are under here, so a step can tell its own
/// from anyone else's.
pub const BRANCHES: &str = "conary/";

/// Where a candidate is built, one merge at a time.
pub fn build_branch(id: CandidateId) -> String {
    format!("{BRANCHES}build/{id}")
}

/// Where its gate runs: made once, at the candidate's commit.
pub fn train_branch(id: CandidateId) -> String {
    format!("{BRANCHES}train/{id}")
}

/// The train's own status context, on the head of each PR it lands or sends
/// away red. A repo's rules can require it, so that nothing lands a train
/// repo's PR but the train.
pub const CONTEXT: &str = "conary/train";

/// What the train's status on a PR's head says. Only the one it's landing
/// now is a success, which a train repo's rules require: every other head
/// the train has had is pending, red or refused, so nothing can merge it
/// outside the train.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// Green in its candidate and landing now: a success.
    Landing,
    /// In the train or out of it, and not landing: pending. It goes on a
    /// head as it joins and leaves, and before its candidate is dropped.
    Waiting,
    /// Red alone in its candidate: a failure.
    Red,
    /// GitHub refused its merge, so it isn't landing after all: an error,
    /// which no repo's rules let land.
    Refused,
}

/// What a PR's merge did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrMerge {
    Merged,
    /// GitHub said no, and why.
    Refused(String),
}

/// What a merge onto a branch did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Merged {
    /// The merge commit, and its tree.
    Commit { sha: String, tree: String },
    /// The branch already holds that head.
    Already,
    /// It won't merge.
    Conflict,
}

/// The train's writes on GitHub, and the reads it needs that ingest
/// doesn't. Branches are named without `refs/heads/`.
pub trait Forge {
    /// The repo's default branch: trunk, which the train builds on and lands
    /// onto, as GitHub says it is now.
    fn default_branch(&self, repo: &str) -> Result<String, ApiError>;
    /// The commit a branch is at, if it's there.
    fn branch(&self, repo: &str, name: &str) -> Result<Option<String>, ApiError>;
    /// Every branch whose name starts with `prefix`.
    fn branches(&self, repo: &str, prefix: &str) -> Result<Vec<String>, ApiError>;
    fn create_branch(&self, repo: &str, name: &str, sha: &str) -> Result<(), ApiError>;
    /// Deletes a branch; one that's already gone is fine.
    fn delete_branch(&self, repo: &str, name: &str) -> Result<(), ApiError>;
    /// Merges `head`, a commit, into the branch `into`.
    fn merge(&self, repo: &str, into: &str, head: &str, message: &str) -> Result<Merged, ApiError>;
    /// A commit's tree.
    fn tree(&self, repo: &str, sha: &str) -> Result<String, ApiError>;
    /// Puts the train's status ([`CONTEXT`]) on a commit.
    fn mark(
        &self,
        repo: &str,
        sha: &str,
        mark: Mark,
        description: &str,
        url: Option<&str>,
    ) -> Result<(), ApiError>;
    /// Merges a PR at `head` the way the repo merges PRs, or not at all.
    fn land(&self, repo: &str, pr: u64, head: &str) -> Result<PrMerge, ApiError>;
}

#[derive(Debug)]
pub enum TrainError {
    Api(ApiError),
    Store(StoreError),
    Ingest(IngestError),
    /// GitHub's answer isn't what it says.
    Malformed(String),
    /// No randomness for a new candidate id.
    Random(String),
    /// The ledger has no change for that PR.
    Unknown(u64),
    /// The PR's head isn't the one the caller said is ready.
    Moved {
        pr: u64,
        head: String,
    },
    /// That candidate isn't this repo's.
    NotHere(CandidateId),
    /// That PR's landing has started and no step has settled it: its merge
    /// may be on trunk already, unseen.
    Landing(u64),
    /// The trunk branch isn't there.
    NoTrunk(String),
}

impl std::fmt::Display for TrainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Api(e) => e.fmt(f),
            Self::Store(e) => e.fmt(f),
            Self::Ingest(e) => e.fmt(f),
            Self::Malformed(what) => write!(f, "GitHub sent {what}"),
            Self::Random(e) => write!(f, "no randomness for a candidate id: {e}"),
            Self::Unknown(pr) => write!(f, "#{pr} isn't in the ledger"),
            Self::Moved { pr, head } => write!(f, "#{pr}'s head is {head} now"),
            Self::NotHere(id) => write!(f, "candidate {id} isn't this repo's"),
            Self::Landing(pr) => {
                write!(f, "#{pr} is landing in it, and only a step can settle that")
            }
            Self::NoTrunk(name) => write!(f, "no trunk branch {name}"),
        }
    }
}

impl std::error::Error for TrainError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Api(e) => Some(e),
            Self::Ingest(e) => Some(e),
            _ => None,
        }
    }
}

impl From<ApiError> for TrainError {
    fn from(e: ApiError) -> Self {
        Self::Api(e)
    }
}

impl From<StoreError> for TrainError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

impl From<IngestError> for TrainError {
    fn from(e: IngestError) -> Self {
        Self::Ingest(e)
    }
}

/// Records `body`; true when it was new, false when the ledger held it.
fn record(store: &mut Store, body: Body) -> Result<bool, TrainError> {
    match store.record(body) {
        Ok(_) => Ok(true),
        Err(StoreError::Invalid(Invalid::Duplicate)) => Ok(false),
        Err(e) => Err(e.into()),
    }
}

fn change_of(store: &Store, repo: &Repo, pr: u64) -> Result<ChangeId, TrainError> {
    Ok(store
        .ledger()
        .by_pr(repo, pr)
        .ok_or(TrainError::Unknown(pr))?
        .id)
}

/// What joining did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Joined {
    Joined,
    /// It was already in at that head.
    Already,
    /// It left as it is, and can't join again until something moves.
    Held(Held),
    /// It targets this branch, not the repo's default one, which is the
    /// train's.
    Elsewhere(String),
}

/// Puts a PR in its repo's train at `head`, the head everything but the
/// gate said can land, once the ledger holds the PR as GitHub shows it now
/// and that is its head. `alone`, it rides in a candidate of its own, which
/// lands only while trunk is still its base: an R2 change. Its head's train
/// status goes to pending first, so a success left from an earlier time
/// in the train can't let anything merge it.
#[allow(clippy::too_many_arguments)]
pub fn join(
    store: &mut Store,
    forge: &dyn Forge,
    source: &dyn Source,
    config: &Config,
    repo: &Repo,
    pr: u64,
    head: &str,
    alone: bool,
) -> Result<Joined, TrainError> {
    let (got, pull) = ingest::pulled(store, source, config, repo, pr)?;
    if !pull.targets_trunk() {
        return Ok(Joined::Elsewhere(pull.base.name));
    }
    let c = store
        .ledger()
        .change(got.change)
        .expect("ingest recorded it");
    let now = &c.revision(got.revision).expect("ingest recorded it").head;
    if now.as_str() != head {
        return Err(TrainError::Moved {
            pr,
            head: now.to_string(),
        });
    }
    let body = Body::Join {
        change: got.change,
        revision: got.revision,
        alone,
    };
    match store.ledger().check(&body) {
        Ok(()) => {}
        Err(Invalid::Duplicate) => return Ok(Joined::Already),
        Err(Invalid::Held(why)) => return Ok(Joined::Held(why)),
        Err(e) => return Err(StoreError::Invalid(e).into()),
    }
    forge.mark(
        &repo.to_string(),
        head,
        Mark::Waiting,
        "in the train, waiting to land",
        None,
    )?;
    store.record(body)?;
    Ok(Joined::Joined)
}

/// What leaving did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leaving {
    Left,
    /// It wasn't in the train.
    NotIn,
    /// It's in the candidate in flight, whose landing of this PR has
    /// started and isn't settled: it stays until a step settles that.
    Landing(u64),
}

/// Takes a PR out of its repo's train, its head's train status pending
/// first. Not while a landing is open, its head saying it's landing with
/// the candidate it's next in, its merge maybe on trunk unseen: then every
/// change the candidate in flight has left to land stays, since leaving
/// would stop the candidate, and of one that stopped while landing, the
/// change it was landing. Only a step settles that.
pub fn leave(
    store: &mut Store,
    forge: &dyn Forge,
    source: &dyn Source,
    repo: &Repo,
    pr: u64,
    why: Left,
) -> Result<Leaving, TrainError> {
    let change = change_of(store, repo, pr)?;
    let body = Body::Leave { change, why };
    match store.ledger().check(&body) {
        Ok(()) => {}
        Err(Invalid::NotInTrain) => return Ok(Leaving::NotIn),
        Err(e) => return Err(StoreError::Invalid(e).into()),
    }
    // In flight, every change the candidate has left to land stays in, as
    // its leaving would stop the candidate; stopped, the one landing.
    if let Some((c, landing)) = open_landing(store, source, repo)?
        && let left = if c.state == State::Green {
            &c.changes[c.landed.len()..]
        } else {
            &c.changes[c.landed.len()..=c.landed.len()]
        }
        && left.iter().any(|m| m.change == change)
    {
        return Ok(Leaving::Landing(landing));
    }
    let member = store
        .ledger()
        .train(repo)
        .and_then(|t| t.joined().iter().find(|m| m.change == change))
        .expect("check: it's in it")
        .clone();
    let head = head_of(store, &member);
    forge.mark(
        &repo.to_string(),
        head.as_str(),
        Mark::Waiting,
        "out of the train",
        None,
    )?;
    store.record(body)?;
    Ok(Leaving::Left)
}

/// Drops a candidate whose gate won't finish, the stall watchdog's last
/// word: its changes wait again as they joined. Green and red are the gate's
/// word, which [`step`] reads, never a hand's. Not while a landing of it is
/// open, its next head saying it's landing: that's a step's to settle.
pub fn drop_candidate(
    store: &mut Store,
    forge: &dyn Forge,
    source: &dyn Source,
    repo: &Repo,
    candidate: CandidateId,
) -> Result<(), TrainError> {
    let Some(c) = store
        .ledger()
        .candidate(candidate)
        .filter(|c| &c.repo == repo)
        .cloned()
    else {
        return Err(TrainError::NotHere(candidate));
    };
    store
        .ledger()
        .check(&Body::Verdict {
            candidate,
            verdict: Verdict::Dropped,
        })
        .map_err(StoreError::Invalid)?;
    if let Some((open, pr)) = open_landing(store, source, repo)?
        && open.id == candidate
    {
        return Err(TrainError::Landing(pr));
    }
    dropped(store, forge, &c, "dropped")
}

/// Records a candidate dropped. A green one (alone, with nothing landed)
/// may have its head saying it's landing, from a step stopped before its
/// merge: that goes to pending first, since the candidate isn't landing it
/// any more.
fn dropped(
    store: &mut Store,
    forge: &dyn Forge,
    c: &Candidate,
    why: &str,
) -> Result<(), TrainError> {
    if c.state == State::Green {
        let next = &c.changes[c.landed.len()];
        let description = format!("candidate {} {why}, waiting in the train again", c.id);
        forge.mark(
            &c.repo.to_string(),
            head_of(store, next).as_str(),
            Mark::Waiting,
            &description,
            None,
        )?;
    }
    store.record(Body::Verdict {
        candidate: c.id,
        verdict: Verdict::Dropped,
    })?;
    Ok(())
}

/// What a step needs to know about the repo.
#[derive(Debug, Clone)]
pub struct Settings {
    /// The most changes a candidate holds.
    pub size: usize,
    /// Checks a candidate's gate waits for even before they report: the
    /// steward's proof jobs, which post nothing until they run.
    pub require: Vec<String>,
}

/// A change to land, at the head its candidate merged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Landing {
    pub pr: u64,
    pub change: ChangeId,
    pub head: CommitId,
}

/// Where the train is after a step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Now {
    /// Nothing waits and nothing's in flight.
    Idle,
    /// The candidate's gate is running, on its train branch.
    Gating {
        candidate: CandidateId,
        branch: String,
        commit: CommitId,
        prs: Vec<u64>,
        /// The checks it hasn't heard from yet.
        waiting: Vec<String>,
    },
    /// Green, and these are still to land, in this order, each at its head:
    /// GitHub took the first's merge and hasn't shown it merged yet. A
    /// candidate that stopped while landing has only that one.
    Landing {
        candidate: CandidateId,
        landings: Vec<Landing>,
    },
}

/// A PR whose head wouldn't merge onto trunk and the changes ahead of it in
/// a candidate: it left the train.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Conflict {
    pub pr: u64,
    /// The PRs merged onto trunk ahead of it: none when it's trunk itself
    /// it won't merge onto.
    pub onto: Vec<u64>,
    /// Trunk's commit then.
    pub base: CommitId,
}

/// A PR whose head merged onto trunk and the changes ahead of it in a
/// candidate without changing their tree: what it does is there already, so
/// its landing would be an empty merge. It left the train, for the steward
/// to close once trunk has what it does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Empty {
    pub pr: u64,
    /// The PRs merged onto trunk ahead of it: none when trunk has what it
    /// does already.
    pub onto: Vec<u64>,
    /// Trunk's commit then.
    pub base: CommitId,
}

/// A failed check on a candidate's commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Failed {
    pub check: String,
    pub url: Option<String>,
}

/// A PR that was red alone in a candidate: it left the train, and its head
/// has the train's status as a failure, naming the checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Red {
    pub pr: u64,
    pub candidate: CandidateId,
    pub failed: Vec<Failed>,
}

/// A PR whose merge GitHub refused: it left the train.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Refused {
    pub pr: u64,
    /// What GitHub said.
    pub why: String,
}

/// What one step did, and where it left the train.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Stepped {
    /// The PRs that left because their heads wouldn't merge, for their PRs
    /// to be told.
    pub conflicts: Vec<Conflict>,
    /// The PRs that left because their merges changed nothing.
    pub empty: Vec<Empty>,
    /// Each candidate whose gate said, in the order they did.
    pub verdicts: Vec<(CandidateId, Verdict)>,
    pub red: Vec<Red>,
    /// The PRs it merged, in order.
    pub landed: Vec<u64>,
    pub refused: Vec<Refused>,
    /// The PRs it took out instead of landing them, as the steward no
    /// longer says they're settled.
    pub unready: Vec<u64>,
    /// Candidates whose landings all came through, with the tree trunk
    /// ended on recorded.
    pub finished: Vec<CandidateId>,
    pub now: Now,
}

/// One step of a repo's train, the whole of what it does on an event. It
/// finishes the candidates whose landings all came through, builds the next
/// candidate when none is in flight, reads the gate of the one gating and
/// says its verdict once its checks have, lands a green one's changes in
/// order, and deletes the branches of every candidate that isn't in flight.
/// It goes on until the train waits on something: a gate still running, a
/// merge GitHub hasn't shown yet, or nothing to do. It holds the ledger's
/// lock throughout, so two steps never build or land at once. Trunk is the
/// repo's default branch as GitHub says it is when the step starts: the
/// branch join, landing and ingest all mean, never a setting of its own.
pub fn step(
    store: &mut Store,
    forge: &dyn Forge,
    source: &dyn Source,
    config: &Config,
    repo: &Repo,
    settings: &Settings,
) -> Result<Stepped, TrainError> {
    let mut stepped = Stepped {
        conflicts: Vec::new(),
        empty: Vec::new(),
        verdicts: Vec::new(),
        red: Vec::new(),
        landed: Vec::new(),
        refused: Vec::new(),
        unready: Vec::new(),
        finished: Vec::new(),
        now: Now::Idle,
    };
    let trunk = forge.default_branch(&repo.to_string())?;
    stepped.now = loop {
        stepped.finished.extend(finish(store, forge, source, repo)?);
        if let Some(now) = stopped_landing(store, forge, source, config, repo, &trunk)? {
            break now;
        }
        let Some(id) = in_flight_or_built(store, forge, repo, &trunk, settings, &mut stepped)?
        else {
            break Now::Idle;
        };
        let c = store.ledger().candidate(id).expect("in flight").clone();
        match c.state {
            State::Gating => {
                let branch = train_branch(id);
                let full = repo.to_string();
                // Made after the candidate was recorded; a step stopped in
                // between makes it now. Whatever else is pushed to it counts
                // for nothing: only the recorded commit's results are read.
                if forge.branch(&full, &branch)?.is_none() {
                    forge.create_branch(&full, &branch, c.commit.as_str())?;
                }
                match gate(store, source, config, settings, &c)? {
                    Gate::Waiting(waiting) => {
                        break Now::Gating {
                            candidate: id,
                            prs: c.changes.iter().map(|m| pr_of(store, m.change)).collect(),
                            branch,
                            commit: c.commit.clone(),
                            waiting,
                        };
                    }
                    Gate::Green => {
                        store.record(Body::Verdict {
                            candidate: id,
                            verdict: Verdict::Green,
                        })?;
                        stepped.verdicts.push((id, Verdict::Green));
                    }
                    Gate::Red(failed) => {
                        // The PR red alone is told first: once the verdict
                        // is in, no step comes back to the candidate.
                        if let [alone] = c.changes.as_slice() {
                            stepped
                                .red
                                .push(red_alone(store, forge, repo, id, alone, failed)?);
                        }
                        store.record(Body::Verdict {
                            candidate: id,
                            verdict: Verdict::Red,
                        })?;
                        stepped.verdicts.push((id, Verdict::Red));
                    }
                }
            }
            State::Green => {
                if let Some(now) =
                    land_next(store, forge, source, config, &trunk, &c, &mut stepped)?
                {
                    break now;
                }
            }
            _ => unreachable!("only a gating or green candidate is in flight"),
        }
    };
    clean(store, forge, repo)?;
    Ok(stepped)
}

fn pr_of(store: &Store, change: ChangeId) -> u64 {
    store.ledger().change(change).expect("in its train").pr
}

fn head_of(store: &Store, m: &Member) -> CommitId {
    store
        .ledger()
        .change(m.change)
        .expect("in its train")
        .revision(m.revision)
        .expect("joined at a revision it has")
        .head
        .clone()
}

/// The candidate in flight, or the next one, built now; none when nothing
/// waits. A change whose head won't merge, or whose merge changes nothing,
/// leaves on the way.
fn in_flight_or_built(
    store: &mut Store,
    forge: &dyn Forge,
    repo: &Repo,
    trunk: &str,
    settings: &Settings,
    stepped: &mut Stepped,
) -> Result<Option<CandidateId>, TrainError> {
    if let Some(id) = store.ledger().train(repo).and_then(|t| t.in_flight()) {
        return Ok(Some(id));
    }
    loop {
        let Some(plan) = store.ledger().next(repo, settings.size) else {
            return Ok(None);
        };
        match build(store, forge, repo, trunk, &plan)? {
            Built::Candidate(id) => return Ok(Some(id)),
            Built::Left {
                change,
                why,
                onto,
                base,
            } => {
                let pr = pr_of(store, change);
                if why == Left::Empty {
                    stepped.empty.push(Empty { pr, onto, base });
                } else {
                    stepped.conflicts.push(Conflict { pr, onto, base });
                }
                store.record(Body::Leave { change, why })?;
            }
        }
    }
}

/// Tells a PR red alone in a candidate why it left: the train's status on
/// its head, a failure naming the checks, for the steward to hand on as it
/// does red CI.
fn red_alone(
    store: &Store,
    forge: &dyn Forge,
    repo: &Repo,
    candidate: CandidateId,
    alone: &Member,
    failed: Vec<Failed>,
) -> Result<Red, TrainError> {
    let names: Vec<&str> = failed.iter().map(|f| f.check.as_str()).collect();
    let description = format!("red alone in candidate {candidate}: {}", names.join(", "));
    forge.mark(
        &repo.to_string(),
        head_of(store, alone).as_str(),
        Mark::Red,
        &description,
        failed.iter().find_map(|f| f.url.as_deref()),
    )?;
    Ok(Red {
        pr: pr_of(store, alone.change),
        candidate,
        failed,
    })
}

/// Records the tree trunk ended on for each candidate whose landings all
/// came through: its last landing's. That landing is retired first
/// ([`land::retired`]).
fn finish(
    store: &mut Store,
    forge: &dyn Forge,
    source: &dyn Source,
    repo: &Repo,
) -> Result<Vec<CandidateId>, TrainError> {
    let ledger = store.ledger();
    let landed: Vec<(CandidateId, CommitId)> = ledger
        .train(repo)
        .map(|t| t.candidates())
        .unwrap_or_default()
        .iter()
        .filter_map(|id| ledger.candidate(*id))
        .filter(|c| c.state == State::Landed)
        .map(|c| {
            let last = c.landed.last().expect("every change landed");
            let landing = ledger
                .change(*last)
                .and_then(|l| l.landing.as_ref())
                .expect("landed through it");
            (c.id, landing.after.clone())
        })
        .collect();
    let mut finished = Vec::new();
    for (id, after) in landed {
        let c = store.ledger().candidate(id).expect("landed").clone();
        land::retired(store, forge, source, &c)?;
        let tree = tree_id(&forge.tree(&repo.to_string(), after.as_str())?)?;
        store.record(Body::Finish {
            candidate: id,
            tree,
        })?;
        finished.push(id);
    }
    Ok(finished)
}

enum Built {
    Candidate(CandidateId),
    /// This change leaves, `why`: its head won't merge onto `base` and the
    /// PRs `onto` (a conflict), or merges onto them without changing their
    /// tree (empty).
    Left {
        change: ChangeId,
        why: Left,
        onto: Vec<u64>,
        base: CommitId,
    },
}

/// Builds the planned candidate on trunk as it is now and records it. Its
/// build branch is left for [`clean`], at the end of the step, like one
/// left by a step that stopped partway.
fn build(
    store: &mut Store,
    forge: &dyn Forge,
    repo: &Repo,
    trunk: &str,
    plan: &conary_core::ledger::Plan,
) -> Result<Built, TrainError> {
    let full = repo.to_string();
    let id = CandidateId::new().map_err(|e| TrainError::Random(e.to_string()))?;
    let base = forge
        .branch(&full, trunk)?
        .ok_or_else(|| TrainError::NoTrunk(trunk.to_owned()))?;
    let build = build_branch(id);
    forge.create_branch(&full, &build, &base)?;
    // The candidate so far: trunk's commit, then each merge's.
    let (mut commit, mut tree) = (base.clone(), forge.tree(&full, &base)?);
    let mut onto = Vec::new();
    for m in &plan.changes {
        let c = store.ledger().change(m.change).expect("in its train");
        let head = &c
            .revision(m.revision)
            .expect("joined at a revision it has")
            .head;
        let message = format!("Merge #{} into the train's candidate {id}", c.pr);
        let left = match forge.merge(&full, &build, head.as_str(), &message)? {
            // The same tree as before it: what it does is there already,
            // and landing it after these would be an empty merge.
            Merged::Commit { tree: after, .. } if after == tree => Some(Left::Empty),
            Merged::Commit { sha, tree: after } => {
                (commit, tree) = (sha, after);
                None
            }
            Merged::Already => None,
            Merged::Conflict => Some(Left::Conflict),
        };
        if let Some(why) = left {
            return Ok(Built::Left {
                change: m.change,
                why,
                onto,
                base: commit_id(&base)?,
            });
        }
        onto.push(c.pr);
    }
    let commit = commit_id(&commit)?;
    store.record(Body::Candidate {
        candidate: id,
        repo: repo.clone(),
        trunk: trunk.to_owned(),
        base: commit_id(&base)?,
        changes: plan.changes.clone(),
        commit: commit.clone(),
        tree: tree_id(&tree)?,
        of: plan.of,
    })?;
    forge.create_branch(&full, &train_branch(id), commit.as_str())?;
    Ok(Built::Candidate(id))
}

/// What a candidate's checks say so far.
enum Gate {
    /// The checks it hasn't heard from yet: none named when no check has
    /// reported at all.
    Waiting(Vec<String>),
    Green,
    Red(Vec<Failed>),
}

/// A check's latest word on a commit.
enum Said {
    Pass,
    Pending,
    Fail(Option<String>),
}

/// Records the CI results on the candidate's commit, as ingest records a
/// PR's, and says what they add up to. Red once any check's latest result
/// is a failure: a failed test has had its second try inside its run
/// (`docs/DESIGN.md` section 4.7). Green once every check has passed and the
/// required ones are among them. Waiting otherwise, with no check at all
/// too, so a gate that never starts is a stall, never a pass.
fn gate(
    store: &mut Store,
    source: &dyn Source,
    config: &Config,
    settings: &Settings,
    c: &Candidate,
) -> Result<Gate, TrainError> {
    let full = c.repo.to_string();
    let runs = source.check_runs(&full, c.commit.as_str())?;
    // Newest first from GitHub; recorded in the order they happened.
    let statuses: Vec<_> = source
        .statuses(&full, c.commit.as_str())?
        .into_iter()
        .filter(|s| config.is_check(&s.context))
        .collect();
    let mut found: Vec<_> = runs.iter().filter_map(run_result).collect();
    found.extend(statuses.iter().rev().filter_map(status_result));
    for f in found {
        record(
            store,
            Body::CandidateEvidence {
                candidate: c.id,
                kind: CheckKind::Ci,
                check: f.check,
                result: f.result,
                source: f.source,
            },
        )?;
    }

    // Each check's latest word. A run gives way only to a rerun of it: a
    // later run (GitHub's ids grow) with its name in its suite, so runs of
    // one name from two workflows or Apps each count. A status gives way to
    // a later one in its context, and statuses and runs never stand in for
    // each other.
    let mut latest: BTreeMap<(Option<u64>, &str), &CheckRun> = BTreeMap::new();
    for run in &runs {
        let check = (run.check_suite.as_ref().map(|s| s.id), run.name.as_str());
        if latest.get(&check).is_none_or(|old| run.id > old.id) {
            latest.insert(check, run);
        }
    }
    let mut said: Vec<(&str, Said)> = latest
        .into_values()
        .map(|run| {
            let s = match (run.status.as_str(), run.conclusion.as_deref()) {
                ("completed", Some("success" | "neutral" | "skipped")) => Said::Pass,
                ("completed", _) => Said::Fail(run.html_url.clone()),
                _ => Said::Pending,
            };
            (run.name.as_str(), s)
        })
        .collect();
    // Newest first from GitHub: a context's first status is its latest.
    let mut contexts = BTreeSet::new();
    for status in &statuses {
        if contexts.insert(status.context.as_str()) {
            let s = match status.state.as_str() {
                "success" => Said::Pass,
                "pending" => Said::Pending,
                _ => Said::Fail(status.target_url.clone()),
            };
            said.push((&status.context, s));
        }
    }
    let failed: Vec<Failed> = said
        .iter()
        .filter_map(|(check, s)| match s {
            Said::Fail(url) => Some(Failed {
                check: (*check).to_owned(),
                url: url.clone(),
            }),
            _ => None,
        })
        .collect();
    if !failed.is_empty() {
        return Ok(Gate::Red(failed));
    }
    let mut waiting: Vec<String> = said
        .iter()
        .filter(|(_, s)| matches!(s, Said::Pending))
        .map(|(check, _)| (*check).to_owned())
        .collect();
    waiting.extend(
        settings
            .require
            .iter()
            .filter(|r| !said.iter().any(|(check, _)| check == r))
            .cloned(),
    );
    Ok(if said.is_empty() || !waiting.is_empty() {
        Gate::Waiting(waiting)
    } else {
        Gate::Green
    })
}

/// Deletes the branches of every candidate that isn't in flight: a build
/// branch always (the train branch has its commit), and a train branch
/// once its candidate is done with.
fn clean(store: &Store, forge: &dyn Forge, repo: &Repo) -> Result<(), TrainError> {
    let full = repo.to_string();
    let keep = store
        .ledger()
        .train(repo)
        .and_then(|t| t.in_flight())
        .map(train_branch);
    for name in forge.branches(&full, BRANCHES)? {
        let ours = name.starts_with(&format!("{BRANCHES}build/"))
            || name.starts_with(&format!("{BRANCHES}train/"));
        if ours && Some(&name) != keep.as_ref() {
            forge.delete_branch(&full, &name)?;
        }
    }
    Ok(())
}

fn commit_id(sha: &str) -> Result<CommitId, TrainError> {
    sha.parse()
        .map_err(|_| TrainError::Malformed(format!("{sha:?} as a commit id")))
}

fn tree_id(sha: &str) -> Result<TreeId, TrainError> {
    sha.parse()
        .map_err(|_| TrainError::Malformed(format!("{sha:?} as a tree id")))
}

#[cfg(test)]
mod tests;
