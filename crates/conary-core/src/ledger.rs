//! The ledger (`docs/DESIGN.md` section 4): every change, revision, piece of
//! evidence and landing, as the op log's ops build it up. It's only ever
//! built by replaying ops, and an op it can't apply is refused before it's
//! written.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::id::{CandidateId, ChangeId, CommitId, OpId, RevisionId};
use crate::op::{Actor, Attest, Authority, Body, CheckKind, Op, Outcome, Repo};
use crate::policy::Class;

mod train;
mod undo;

pub use train::{Candidate, CandidateEvidence, Held, Plan, State, Train};
pub use undo::Undo;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Intent {
    pub summary: String,
    pub body: String,
    pub task: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Revision {
    pub id: RevisionId,
    pub base: CommitId,
    pub head: CommitId,
    pub at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Evidence {
    pub revision: RevisionId,
    pub kind: CheckKind,
    pub check: String,
    pub result: Outcome,
    pub source: String,
    /// Its producer's attestation, when it had one that held.
    pub attest: Option<Attest>,
    pub at: i64,
}

/// What the class policy said of one revision (design 4.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Classed {
    pub revision: RevisionId,
    pub class: Class,
    /// What made it R2.
    pub because: Option<String>,
    /// Every path it touches is documentation.
    pub docs: bool,
    /// The BLAKE3 hash of the policy file that said it.
    pub policy: String,
    /// The labels its PR raised it with, sorted.
    pub raised: Vec<String>,
    pub at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Landing {
    pub revision: RevisionId,
    pub before: CommitId,
    pub after: CommitId,
    pub by: Authority,
    pub at: i64,
    /// The green candidate it landed through, when it came through the train.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate: Option<CandidateId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Open,
    Closed,
    Landed,
    /// Landed, and an undo of it has landed since, itself not undone.
    Reverted,
}

impl Status {
    /// Every status, as the RPC's filter takes them.
    pub const ALL: [Status; 4] = [
        Status::Open,
        Status::Closed,
        Status::Landed,
        Status::Reverted,
    ];
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Change {
    pub id: ChangeId,
    pub repo: Repo,
    pub pr: u64,
    pub author: Actor,
    pub seen: i64,
    pub intent: Option<Intent>,
    /// Each revision once, in the order first seen. A head that comes back
    /// after a force-push is the revision it already was.
    pub revisions: Vec<Revision>,
    /// The revision the PR is at, as last seen: the one it came back to, if
    /// it did.
    pub current: Option<RevisionId>,
    pub evidence: Vec<Evidence>,
    /// What the class policy said of its revisions, oldest first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub classes: Vec<Classed>,
    pub landing: Option<Landing>,
    /// What it reverts, when it's an undo.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub undo: Option<Undo>,
    /// The undos recorded of it, oldest first, landed or not.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub undone_by: Vec<ChangeId>,
    /// The landed undo of it in effect, one that hasn't been undone itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reverted_by: Option<ChangeId>,
    #[serde(skip)]
    closed: bool,
    /// The last op recorded about it: [`Change::last`].
    #[serde(skip)]
    last: Option<OpId>,
}

impl Change {
    pub fn status(&self) -> Status {
        match (&self.landing, self.closed) {
            (Some(_), _) if self.reverted_by.is_some() => Status::Reverted,
            (Some(_), _) => Status::Landed,
            (None, true) => Status::Closed,
            (None, false) => Status::Open,
        }
    }

    /// The change's pull request on GitHub.
    pub fn url(&self) -> String {
        format!("https://github.com/{}/pull/{}", self.repo, self.pr)
    }

    pub fn revision(&self, id: RevisionId) -> Option<&Revision> {
        self.revisions.iter().find(|r| r.id == id)
    }

    /// What the class policy last said of a revision, if it's been asked.
    pub fn class_of(&self, revision: RevisionId) -> Option<&Classed> {
        self.classes.iter().rev().find(|c| c.revision == revision)
    }

    /// The last op recorded about it. Every op names its parent, so no two
    /// are the same, and while a change's last op is the same no op about
    /// it has been recorded since, even one that set it back as it was:
    /// a title edited and then put back, say.
    pub fn last(&self) -> Option<OpId> {
        self.last
    }

    /// Every label that has raised the change, at any revision, sorted. A
    /// raise stays once it's recorded: taking the label off lowers nothing.
    pub fn raised(&self) -> Vec<String> {
        crate::policy::raises(
            self.classes
                .iter()
                .flat_map(|k| &k.raised)
                .map(String::as_str),
        )
    }
}

/// Trunk moving some way other than `land`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Outside {
    pub repo: Repo,
    pub before: Option<CommitId>,
    pub after: CommitId,
    pub at: i64,
}

/// Why an op can't be applied. Nothing is written for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invalid {
    UnknownChange(ChangeId),
    /// An op that would record what the ledger already holds.
    Duplicate,
    /// The change id or the PR is already in the ledger.
    AlreadyObserved,
    UnknownRevision(RevisionId),
    /// That revision isn't the PR's head now.
    NotCurrent(RevisionId),
    AlreadyLanded,
    Closed,
    NotClosed,
    /// That trunk commit is already some other landing's.
    TrunkCommitTaken(CommitId),
    UnknownCandidate(CandidateId),
    NotInTrain,
    /// The repo already has a candidate in flight.
    TrainBusy,
    /// Not the candidate the train builds next.
    OutOfTurn,
    /// The candidate's gate already ended.
    NotGating,
    /// Not every change in the candidate has landed through it.
    NotLanded,
    /// The change left its train and can't join again as it is.
    Held(Held),
    /// An undo that reverts nothing.
    NothingToUndo,
    /// A new undo lacks its nonblank operational reason.
    UndoReason,
    /// The change is an undo already.
    AlreadyUndo,
    /// An undo can't revert itself, or a change twice.
    UndoRepeats(ChangeId),
    /// That change is in another repo than its undo.
    OtherRepo(ChangeId),
    /// That change hasn't landed, so there's nothing to revert.
    NotLandedYet(ChangeId),
    /// That change already has an undo, open or landed.
    AlreadyUndone {
        change: ChangeId,
        by: ChangeId,
    },
    /// That change's undos, followed down, revert the undo itself.
    UndoCycle(ChangeId),
    /// That change isn't one of the ones the undo was asked for.
    NotAsked(ChangeId),
    /// A join below its revision's class: an R2 one that doesn't ride alone.
    BelowClass(RevisionId),
    /// A class its raise labels don't account for: one that isn't a raise
    /// label, a list out of order, `r2` on a class below R2, or a label
    /// that raised the change before left out.
    NotRaised,
    /// A join that doesn't name exactly the labels recorded as raising its
    /// change.
    JoinRaised,
}

impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownChange(id) => write!(f, "no change {id}"),
            Self::Duplicate => f.write_str("already recorded"),
            Self::AlreadyObserved => f.write_str("that change or PR is already in the ledger"),
            Self::UnknownRevision(id) => write!(f, "no revision {id} on that change"),
            Self::NotCurrent(id) => write!(f, "revision {id} isn't the change's head now"),
            Self::AlreadyLanded => f.write_str("the change already landed"),
            Self::Closed => f.write_str("the change is closed"),
            Self::NotClosed => f.write_str("the change isn't closed"),
            Self::TrunkCommitTaken(c) => write!(f, "trunk commit {c} already belongs to a landing"),
            Self::UnknownCandidate(id) => write!(f, "no candidate {id}"),
            Self::NotInTrain => f.write_str("the change isn't in its repo's train"),
            Self::TrainBusy => f.write_str("that repo's train already has a candidate in flight"),
            Self::OutOfTurn => f.write_str("that isn't the candidate the train builds next"),
            Self::NotGating => f.write_str("that candidate's gate already ended"),
            Self::NotLanded => {
                f.write_str("not every change in that candidate has landed through it")
            }
            Self::Held(why) => write!(f, "the change can't join its train again: {why}"),
            Self::NothingToUndo => f.write_str("the undo reverts nothing"),
            Self::UndoReason => f.write_str("an undo requires a nonblank reason"),
            Self::AlreadyUndo => f.write_str("the change is an undo already"),
            Self::UndoRepeats(id) => write!(f, "the undo names {id} twice, or itself"),
            Self::OtherRepo(id) => write!(f, "change {id} is in another repo than its undo"),
            Self::NotLandedYet(id) => write!(f, "change {id} hasn't landed"),
            Self::AlreadyUndone { change, by } => {
                write!(f, "change {change} already has an undo, {by}")
            }
            Self::NotAsked(id) => write!(f, "change {id} isn't what the undo was asked for"),
            Self::BelowClass(id) => write!(f, "revision {id} is R2, so it rides alone"),
            Self::NotRaised => f.write_str("its raise labels don't match its class"),
            Self::JoinRaised => {
                f.write_str("the join doesn't name the labels recorded as raising its change")
            }
            Self::UndoCycle(id) => {
                write!(
                    f,
                    "change {id}'s undos, followed down, revert the undo itself"
                )
            }
        }
    }
}

impl std::error::Error for Invalid {}

#[derive(Debug, Default, Clone)]
pub struct Ledger {
    changes: BTreeMap<ChangeId, Change>,
    /// Change ids in the order they were observed.
    seen: Vec<ChangeId>,
    by_pr: HashMap<(Repo, u64), ChangeId>,
    by_trunk: HashMap<(Repo, CommitId), ChangeId>,
    outside: Vec<Outside>,
    trains: HashMap<Repo, Train>,
    candidates: BTreeMap<CandidateId, Candidate>,
    head: Option<OpId>,
}

impl Ledger {
    /// The ledger the ops build, in order. A signed log holding an op that
    /// doesn't apply is refused, like a log that doesn't verify.
    pub fn replay<'a>(ops: impl IntoIterator<Item = &'a Op>) -> Result<Self, (OpId, Invalid)> {
        let mut ledger = Self::default();
        for op in ops {
            ledger.check(&op.op.body).map_err(|e| (op.id, e))?;
            ledger.apply(op);
        }
        Ok(ledger)
    }

    /// The last op applied.
    pub fn head(&self) -> Option<OpId> {
        self.head
    }

    pub fn change(&self, id: ChangeId) -> Option<&Change> {
        self.changes.get(&id)
    }

    pub fn by_pr(&self, repo: &Repo, pr: u64) -> Option<&Change> {
        self.by_pr
            .get(&(repo.clone(), pr))
            .and_then(|id| self.changes.get(id))
    }

    /// The change whose landing made `commit` on `repo`'s trunk.
    pub fn landed_as(&self, repo: &Repo, commit: &CommitId) -> Option<&Change> {
        self.by_trunk
            .get(&(repo.clone(), commit.clone()))
            .and_then(|id| self.changes.get(id))
    }

    /// Every change, in the order the ledger first saw them.
    pub fn changes(&self) -> impl Iterator<Item = &Change> {
        self.seen.iter().map(|id| &self.changes[id])
    }

    pub fn outside(&self) -> &[Outside] {
        &self.outside
    }

    fn existing(&self, id: ChangeId) -> Result<&Change, Invalid> {
        self.changes.get(&id).ok_or(Invalid::UnknownChange(id))
    }

    /// Whether `body` can be recorded now: [`Ledger::check`], and the rules
    /// a new op is held to that a signed log may hold older ops from before
    /// (which [`Ledger::replay`] still applies, as they were written).
    pub fn admit(&self, body: &Body) -> Result<(), Invalid> {
        self.check(body)?;
        match body {
            Body::Undo { reason: None, .. } => Err(Invalid::UndoReason),
            Body::Join {
                change,
                revision,
                alone,
                ..
            } => self.admit_join(*change, *revision, *alone),
            _ => Ok(()),
        }
    }

    /// Whether `body` can be applied now.
    pub fn check(&self, body: &Body) -> Result<(), Invalid> {
        match body {
            Body::Observe {
                change, repo, pr, ..
            } => {
                if self.changes.contains_key(change)
                    || self.by_pr.contains_key(&(repo.clone(), *pr))
                {
                    return Err(Invalid::AlreadyObserved);
                }
            }
            Body::Intent {
                change,
                summary,
                body,
                task,
            } => {
                let current = self.existing(*change)?.intent.as_ref();
                let new = (summary, body, task);
                if current.is_some_and(|i| (&i.summary, &i.body, &i.task) == new) {
                    return Err(Invalid::Duplicate);
                }
            }
            Body::Revision { change, base, head } => {
                if self.existing(*change)?.current == Some(RevisionId::new(base, head)) {
                    return Err(Invalid::Duplicate);
                }
            }
            Body::Evidence {
                change,
                revision,
                kind,
                check,
                result,
                source,
                attest,
            } => {
                let c = self.existing(*change)?;
                if c.revision(*revision).is_none() {
                    return Err(Invalid::UnknownRevision(*revision));
                }
                // The same result attested is more than it was without.
                let same = |e: &&Evidence| {
                    (
                        &e.revision,
                        &e.kind,
                        &e.check,
                        &e.result,
                        &e.source,
                        &e.attest,
                    ) == (revision, kind, check, result, source, attest)
                };
                if c.evidence.iter().any(|e| same(&e)) {
                    return Err(Invalid::Duplicate);
                }
            }
            Body::Classify {
                change,
                revision,
                class,
                because,
                docs,
                policy,
                raised,
            } => {
                let c = self.existing(*change)?;
                if c.revision(*revision).is_none() {
                    return Err(Invalid::UnknownRevision(*revision));
                }
                if *raised != crate::policy::raises(raised.iter().map(String::as_str))
                    || (raised.iter().any(|l| l == crate::policy::R2_LABEL) && *class != Class::R2)
                    || c.raised().iter().any(|l| !raised.contains(l))
                {
                    return Err(Invalid::NotRaised);
                }
                if c.class_of(*revision).is_some_and(|k| {
                    (&k.class, &k.because, &k.docs, &k.policy, &k.raised)
                        == (class, because, docs, policy, raised)
                }) {
                    return Err(Invalid::Duplicate);
                }
            }
            Body::Land {
                change,
                revision,
                after,
                ..
            } => {
                let c = self.existing(*change)?;
                match c.status() {
                    Status::Landed | Status::Reverted => return Err(Invalid::AlreadyLanded),
                    Status::Closed => return Err(Invalid::Closed),
                    Status::Open => {}
                }
                if c.revision(*revision).is_none() {
                    return Err(Invalid::UnknownRevision(*revision));
                }
                if self.by_trunk.contains_key(&(c.repo.clone(), after.clone())) {
                    return Err(Invalid::TrunkCommitTaken(after.clone()));
                }
            }
            Body::Outside { .. } => {}
            Body::Close { change } => match self.existing(*change)?.status() {
                Status::Landed | Status::Reverted => return Err(Invalid::AlreadyLanded),
                Status::Closed => return Err(Invalid::Closed),
                Status::Open => {}
            },
            Body::Reopen { change } => {
                if self.existing(*change)?.status() != Status::Closed {
                    return Err(Invalid::NotClosed);
                }
                self.check_reopen(*change)?;
            }
            Body::Join {
                change,
                revision,
                raised,
                ..
            } => self.check_join(*change, *revision, raised)?,
            Body::Leave { change, .. } => self.check_leave(*change)?,
            Body::Candidate {
                candidate,
                repo,
                changes,
                of,
                ..
            } => self.check_candidate(*candidate, repo, changes, *of)?,
            Body::CandidateEvidence {
                candidate,
                kind,
                check,
                result,
                source,
                attest,
            } => self.check_candidate_evidence(
                *candidate,
                *kind,
                check,
                *result,
                source,
                attest.as_ref(),
            )?,
            Body::Verdict { candidate, verdict } => self.check_verdict(*candidate, *verdict)?,
            Body::Finish { candidate, .. } => self.check_finish(*candidate)?,
            Body::Undo {
                change,
                reverts,
                asked,
                reason,
            } => self.check_undo(*change, reverts, asked, reason.as_deref())?,
        }
        Ok(())
    }

    /// Applies an op [`Ledger::check`] has accepted.
    pub(crate) fn apply(&mut self, op: &Op) {
        self.head = Some(op.id);
        self.apply_body(op);
        if let Some(c) = op.op.body.change().and_then(|id| self.changes.get_mut(&id)) {
            c.last = Some(op.id);
        }
    }

    fn apply_body(&mut self, op: &Op) {
        let at = op.op.at;
        if self.apply_train(&op.op.body, at) || self.apply_undo(&op.op.body, at) {
            return;
        }
        // What a change's new head, close or landing does to its train.
        let through = match &op.op.body {
            Body::Revision { change, .. } | Body::Close { change } => {
                self.unseat(*change);
                None
            }
            Body::Land {
                change,
                revision,
                before,
                ..
            } => self.land_through_train(*change, *revision, before),
            _ => None,
        };
        let change = op.op.body.change();
        let Some(c) = change.and_then(|id| self.changes.get_mut(&id)) else {
            match &op.op.body {
                Body::Observe {
                    change,
                    repo,
                    pr,
                    author,
                } => {
                    self.by_pr.insert((repo.clone(), *pr), *change);
                    self.seen.push(*change);
                    self.changes.insert(
                        *change,
                        Change {
                            id: *change,
                            repo: repo.clone(),
                            pr: *pr,
                            author: author.clone(),
                            seen: at,
                            intent: None,
                            revisions: Vec::new(),
                            current: None,
                            evidence: Vec::new(),
                            classes: Vec::new(),
                            landing: None,
                            undo: None,
                            undone_by: Vec::new(),
                            reverted_by: None,
                            closed: false,
                            last: None,
                        },
                    );
                }
                Body::Outside {
                    repo,
                    before,
                    after,
                } => {
                    if let Some(train) = self.trains.get_mut(repo) {
                        train.moved();
                    }
                    self.outside.push(Outside {
                        repo: repo.clone(),
                        before: before.clone(),
                        after: after.clone(),
                        at,
                    })
                }
                _ => unreachable!("check refuses an op about an unknown change"),
            }
            return;
        };
        match &op.op.body {
            Body::Intent {
                summary,
                body,
                task,
                ..
            } => {
                c.intent = Some(Intent {
                    summary: summary.clone(),
                    body: body.clone(),
                    task: task.clone(),
                })
            }
            Body::Revision { base, head, .. } => {
                let id = RevisionId::new(base, head);
                if c.revision(id).is_none() {
                    c.revisions.push(Revision {
                        id,
                        base: base.clone(),
                        head: head.clone(),
                        at,
                    });
                }
                c.current = Some(id);
            }
            Body::Evidence {
                revision,
                kind,
                check,
                result,
                source,
                attest,
                ..
            } => c.evidence.push(Evidence {
                revision: *revision,
                kind: *kind,
                check: check.clone(),
                result: *result,
                source: source.clone(),
                attest: attest.clone(),
                at,
            }),
            Body::Classify {
                revision,
                class,
                because,
                docs,
                policy,
                raised,
                ..
            } => c.classes.push(Classed {
                revision: *revision,
                class: *class,
                because: because.clone(),
                docs: *docs,
                policy: policy.clone(),
                raised: raised.clone(),
                at,
            }),
            Body::Land {
                revision,
                before,
                after,
                by,
                ..
            } => {
                self.by_trunk.insert((c.repo.clone(), after.clone()), c.id);
                c.landing = Some(Landing {
                    revision: *revision,
                    before: before.clone(),
                    after: after.clone(),
                    by: by.clone(),
                    at,
                    candidate: through,
                });
            }
            Body::Close { .. } => c.closed = true,
            Body::Reopen { .. } => c.closed = false,
            Body::Observe { .. } | Body::Outside { .. } => {
                unreachable!("check refuses observing a change twice; outside names none")
            }
            Body::Join { .. }
            | Body::Leave { .. }
            | Body::Candidate { .. }
            | Body::CandidateEvidence { .. }
            | Body::Verdict { .. }
            | Body::Finish { .. } => unreachable!("apply_train applies the train's ops"),
            Body::Undo { .. } => unreachable!("apply_undo applies undo"),
        }
        // An undo that lands reverts what it names.
        if let Body::Land { change, .. } = &op.op.body {
            self.landed_undo(*change);
        }
    }
}
