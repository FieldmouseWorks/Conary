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

use std::collections::BTreeMap;

use serde::Serialize;

use conary_core::ledger::{Candidate, Held, Invalid, State};
use conary_core::op::{Body, CheckKind, Left, Member, Repo, Verdict};
use conary_core::store::StoreError;
use conary_core::{CandidateId, ChangeId, CommitId, Store, TreeId};

use crate::api::{ApiError, CheckRun, Source};
use crate::ingest::{self, Config, IngestError, run_result, status_result};

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

/// What the train's status on a PR's head says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// Green in its candidate and landing now: a success.
    Landing,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Joined {
    Joined,
    /// It was already in at that head.
    Already,
    /// It left as it is, and can't join again until something moves.
    Held(Held),
}

/// Puts a PR in its repo's train at `head`, the head everything but the
/// gate said can land, once the ledger holds the PR as GitHub shows it now
/// and that is its head. `alone`, it rides in a candidate of its own, which
/// lands only while trunk is still its base: an R2 change.
pub fn join(
    store: &mut Store,
    source: &dyn Source,
    config: &Config,
    repo: &Repo,
    pr: u64,
    head: &str,
    alone: bool,
) -> Result<Joined, TrainError> {
    let got = ingest::pull(store, source, config, repo, pr)?;
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
    match store.record(Body::Join {
        change: got.change,
        revision: got.revision,
        alone,
    }) {
        Ok(_) => Ok(Joined::Joined),
        Err(StoreError::Invalid(Invalid::Duplicate)) => Ok(Joined::Already),
        Err(StoreError::Invalid(Invalid::Held(why))) => Ok(Joined::Held(why)),
        Err(e) => Err(e.into()),
    }
}

/// Takes a PR out of its repo's train. False when it wasn't in it.
pub fn leave(store: &mut Store, repo: &Repo, pr: u64, why: Left) -> Result<bool, TrainError> {
    let change = change_of(store, repo, pr)?;
    match store.record(Body::Leave { change, why }) {
        Ok(_) => Ok(true),
        Err(StoreError::Invalid(Invalid::NotInTrain)) => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Records how the candidate's gate ended.
pub fn verdict(
    store: &mut Store,
    repo: &Repo,
    candidate: CandidateId,
    verdict: Verdict,
) -> Result<(), TrainError> {
    let here = store
        .ledger()
        .candidate(candidate)
        .is_some_and(|c| &c.repo == repo);
    if !here {
        return Err(TrainError::NotHere(candidate));
    }
    store.record(Body::Verdict { candidate, verdict })?;
    Ok(())
}

/// What a step needs to know about the repo.
#[derive(Debug, Clone)]
pub struct Settings {
    /// The trunk branch, the one candidates are built on.
    pub trunk: String,
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
    /// GitHub took the first's merge and hasn't shown it merged yet.
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
    /// Each candidate whose gate said, in the order they did.
    pub verdicts: Vec<(CandidateId, Verdict)>,
    pub red: Vec<Red>,
    /// The PRs it merged, in order.
    pub landed: Vec<u64>,
    pub refused: Vec<Refused>,
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
/// lock throughout, so two steps never build or land at once.
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
        verdicts: Vec::new(),
        red: Vec::new(),
        landed: Vec::new(),
        refused: Vec::new(),
        finished: Vec::new(),
        now: Now::Idle,
    };
    stepped.now = loop {
        stepped.finished.extend(finish(store, forge, repo)?);
        let Some(id) = in_flight_or_built(store, forge, repo, settings, &mut stepped)? else {
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
                        store.record(Body::Verdict {
                            candidate: id,
                            verdict: Verdict::Red,
                        })?;
                        stepped.verdicts.push((id, Verdict::Red));
                        if let [alone] = c.changes.as_slice() {
                            stepped
                                .red
                                .push(red_alone(store, forge, repo, id, alone, failed)?);
                        }
                    }
                }
            }
            State::Green => {
                if let Some(now) = land_next(
                    store,
                    forge,
                    source,
                    config,
                    &settings.trunk,
                    &c,
                    &mut stepped,
                )? {
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
/// waits. A change whose head won't merge leaves on the way.
fn in_flight_or_built(
    store: &mut Store,
    forge: &dyn Forge,
    repo: &Repo,
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
        match build(store, forge, repo, &settings.trunk, &plan)? {
            Built::Candidate(id) => return Ok(Some(id)),
            Built::Conflict { change, onto, base } => {
                stepped.conflicts.push(Conflict {
                    pr: pr_of(store, change),
                    onto,
                    base,
                });
                store.record(Body::Leave {
                    change,
                    why: Left::Conflict,
                })?;
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

/// Lands a green candidate's next change, through its PR's merge at the head
/// the candidate holds. The PR is read first, so a merge made since the last
/// step is recorded rather than tried again. A candidate alone lands only
/// while trunk is still its base: once trunk has moved it's dropped, to be
/// built again on trunk as it is. Then the train's status goes on its head,
/// which the repo's rules can require, and GitHub merges it or refuses.
/// `Some` when the train waits on GitHub showing the merge.
fn land_next(
    store: &mut Store,
    forge: &dyn Forge,
    source: &dyn Source,
    config: &Config,
    trunk: &str,
    c: &Candidate,
    stepped: &mut Stepped,
) -> Result<Option<Now>, TrainError> {
    let repo = &c.repo;
    let next = &c.changes[c.landed.len()];
    let pr = pr_of(store, next.change);
    let head = head_of(store, next);
    let moved = |store: &Store| {
        store
            .ledger()
            .candidate(c.id)
            .is_none_or(|now| now.state != State::Green || now.landed.len() > c.landed.len())
    };
    ingest::pull(store, source, config, repo, pr)?;
    if moved(store) {
        return Ok(None);
    }
    let full = repo.to_string();
    if c.alone {
        let at = forge
            .branch(&full, trunk)?
            .ok_or_else(|| TrainError::NoTrunk(trunk.to_owned()))?;
        if at != c.base.as_str() {
            store.record(Body::Verdict {
                candidate: c.id,
                verdict: Verdict::Dropped,
            })?;
            stepped.verdicts.push((c.id, Verdict::Dropped));
            return Ok(None);
        }
    }
    let description = format!("landing with candidate {}", c.id);
    forge.mark(&full, head.as_str(), Mark::Landing, &description, None)?;
    match forge.land(&full, pr, head.as_str())? {
        PrMerge::Merged => {
            stepped.landed.push(pr);
            ingest::pull(store, source, config, repo, pr)?;
            if moved(store) {
                return Ok(None);
            }
            Ok(Some(Now::Landing {
                candidate: c.id,
                landings: c.changes[c.landed.len()..]
                    .iter()
                    .map(|m| Landing {
                        pr: pr_of(store, m.change),
                        change: m.change,
                        head: head_of(store, m),
                    })
                    .collect(),
            }))
        }
        PrMerge::Refused(why) => {
            store.record(Body::Leave {
                change: next.change,
                why: Left::Refused,
            })?;
            // Its head said it was landing; it isn't, and nothing else may.
            let description = format!("GitHub refused its merge: {why}");
            forge.mark(&full, head.as_str(), Mark::Refused, &description, None)?;
            stepped.refused.push(Refused { pr, why });
            Ok(None)
        }
    }
}

/// Records the tree trunk ended on for each candidate whose landings all
/// came through: its last landing's.
fn finish(
    store: &mut Store,
    forge: &dyn Forge,
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
    /// This change's head won't merge onto `base` and the PRs `onto`.
    Conflict {
        change: ChangeId,
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
    let mut at = None;
    let mut onto = Vec::new();
    for m in &plan.changes {
        let c = store.ledger().change(m.change).expect("in its train");
        let head = &c
            .revision(m.revision)
            .expect("joined at a revision it has")
            .head;
        let message = format!("Merge #{} into the train's candidate {id}", c.pr);
        match forge.merge(&full, &build, head.as_str(), &message)? {
            Merged::Commit { sha, tree } => at = Some((sha, tree)),
            Merged::Already => {}
            Merged::Conflict => {
                return Ok(Built::Conflict {
                    change: m.change,
                    onto,
                    base: commit_id(&base)?,
                });
            }
        }
        onto.push(c.pr);
    }
    let (commit, tree) = match at {
        Some(at) => at,
        None => {
            let tree = forge.tree(&full, &base)?;
            (base.clone(), tree)
        }
    };
    let commit = commit_id(&commit)?;
    store.record(Body::Candidate {
        candidate: id,
        repo: repo.clone(),
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

    // Each check's latest: a rerun replaces the run before it, and a status
    // the one before it in its context. A run not started yet is the newest.
    let mut latest: BTreeMap<&str, &CheckRun> = BTreeMap::new();
    let newness = |r: &CheckRun| (r.started_at.is_none(), r.started_at.clone());
    for run in &runs {
        let newer = latest
            .get(run.name.as_str())
            .is_none_or(|old| newness(run) >= newness(old));
        if newer {
            latest.insert(&run.name, run);
        }
    }
    let mut said: BTreeMap<&str, Said> = latest
        .into_iter()
        .map(|(name, run)| {
            let s = match (run.status.as_str(), run.conclusion.as_deref()) {
                ("completed", Some("success" | "neutral" | "skipped")) => Said::Pass,
                ("completed", _) => Said::Fail(run.html_url.clone()),
                _ => Said::Pending,
            };
            (name, s)
        })
        .collect();
    for status in &statuses {
        said.entry(&status.context)
            .or_insert_with(|| match status.state.as_str() {
                "success" => Said::Pass,
                "pending" => Said::Pending,
                _ => Said::Fail(status.target_url.clone()),
            });
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
            .filter(|r| !said.contains_key(r.as_str()))
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
