//! The train on GitHub (`docs/DESIGN.md` section 4.7). The ledger says what
//! a repo's train does next ([`Ledger::next`]); this does it on GitHub,
//! behind [`Forge`], and records each step. A candidate is built with the
//! merges API on its build branch, so nothing on the box runs git, and the
//! gate runs on its train branch, made at the finished commit. Who says
//! green is the steward's, from the train branch's CI as it reads a PR's,
//! until Conary counts attested evidence itself.
//!
//! [`Ledger::next`]: conary_core::Ledger::next

use serde::Serialize;

use conary_core::ledger::{Candidate, Invalid, State};
use conary_core::op::{Body, CheckKind, Left, Repo, Verdict};
use conary_core::store::StoreError;
use conary_core::{CandidateId, ChangeId, CommitId, Store, TreeId};

use crate::api::{ApiError, Source};
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

/// Puts a PR in its repo's train at `head`, the head everything but the
/// gate said can land, once the ledger holds the PR as GitHub shows it now
/// and that is its head. True when it joined, false when it was already in
/// at that head.
pub fn join(
    store: &mut Store,
    source: &dyn Source,
    config: &Config,
    repo: &Repo,
    pr: u64,
    head: &str,
) -> Result<bool, TrainError> {
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
    record(
        store,
        Body::Join {
            change: got.change,
            revision: got.revision,
        },
    )
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
    },
    /// Green: these land next, in this order, each at its head.
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

/// What one step did, and where it left the train.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Stepped {
    /// The PRs that left because their heads wouldn't merge, for their PRs
    /// to be told.
    pub conflicts: Vec<Conflict>,
    /// Candidates whose landings all came through, with the tree trunk
    /// ended on recorded.
    pub finished: Vec<CandidateId>,
    pub now: Now,
}

/// One step of a repo's train: finishes the candidates whose landings all
/// came through, builds the next candidate if none is in flight, records
/// the CI results on the one gating, and deletes the branches of every
/// candidate that isn't. A step is the whole of what the train does on an
/// event, so it holds the ledger's lock throughout, and two steps never
/// build at once.
pub fn step(
    store: &mut Store,
    forge: &dyn Forge,
    source: &dyn Source,
    config: &Config,
    repo: &Repo,
    settings: &Settings,
) -> Result<Stepped, TrainError> {
    let full = repo.to_string();
    let finished = finish(store, forge, repo)?;
    let mut conflicts = Vec::new();
    let in_flight = store.ledger().train(repo).and_then(|t| t.in_flight());
    let id = match in_flight {
        Some(id) => id,
        None => loop {
            let Some(plan) = store.ledger().next(repo, settings.size) else {
                clean(store, forge, repo)?;
                return Ok(Stepped {
                    conflicts,
                    finished,
                    now: Now::Idle,
                });
            };
            match build(store, forge, repo, &settings.trunk, &plan)? {
                Built::Candidate(id) => break id,
                Built::Conflict { change, onto, base } => {
                    conflicts.push(Conflict {
                        pr: store.ledger().change(change).expect("in its train").pr,
                        onto,
                        base,
                    });
                    store.record(Body::Leave {
                        change,
                        why: Left::Conflict,
                    })?;
                }
            }
        },
    };
    let c = store.ledger().candidate(id).expect("in flight").clone();
    let now = match c.state {
        State::Gating => {
            let branch = train_branch(id);
            // Made after the candidate was recorded; a step stopped in
            // between makes it now. Whatever else is pushed to it counts
            // for nothing: only the recorded commit's results are read.
            if forge.branch(&full, &branch)?.is_none() {
                forge.create_branch(&full, &branch, c.commit.as_str())?;
            }
            gate_results(store, source, config, &c)?;
            Now::Gating {
                candidate: id,
                prs: c
                    .changes
                    .iter()
                    .map(|m| store.ledger().change(m.change).expect("in its train").pr)
                    .collect(),
                branch,
                commit: c.commit.clone(),
            }
        }
        State::Green => Now::Landing {
            candidate: id,
            landings: c
                .changes
                .iter()
                .filter(|m| !c.landed.contains(&m.change))
                .map(|m| {
                    let change = store.ledger().change(m.change).expect("in its train");
                    Landing {
                        pr: change.pr,
                        change: m.change,
                        head: change
                            .revision(m.revision)
                            .expect("joined at a revision it has")
                            .head
                            .clone(),
                    }
                })
                .collect(),
        },
        _ => unreachable!("only a gating or green candidate is in flight"),
    };
    clean(store, forge, repo)?;
    Ok(Stepped {
        conflicts,
        finished,
        now,
    })
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

/// Records the CI results on the candidate's commit, as ingest records a
/// PR's, the steward's own statuses left out.
fn gate_results(
    store: &mut Store,
    source: &dyn Source,
    config: &Config,
    c: &Candidate,
) -> Result<(), TrainError> {
    let full = c.repo.to_string();
    let mut found: Vec<_> = source
        .check_runs(&full, c.commit.as_str())?
        .iter()
        .filter_map(run_result)
        .collect();
    // Newest first from GitHub; recorded in the order they happened.
    let statuses = source.statuses(&full, c.commit.as_str())?;
    found.extend(
        statuses
            .iter()
            .rev()
            .filter(|s| !config.own_contexts.contains(&s.context))
            .filter_map(status_result),
    );
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
    Ok(())
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
