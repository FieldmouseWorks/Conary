//! The ledger (`docs/DESIGN.md` section 4): every change, revision, piece of
//! evidence and landing, as the op log's ops build it up. It's only ever
//! built by replaying ops, and an op it can't apply is refused before it's
//! written.

use std::collections::{BTreeMap, HashMap};

use serde::Serialize;

use crate::id::{ChangeId, CommitId, OpId, RevisionId};
use crate::op::{Actor, Authority, Body, CheckKind, Op, Outcome, Repo};

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
    pub at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Landing {
    pub revision: RevisionId,
    pub before: CommitId,
    pub after: CommitId,
    pub by: Authority,
    pub at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Open,
    Closed,
    Landed,
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
    pub evidence: Vec<Evidence>,
    pub landing: Option<Landing>,
    #[serde(skip)]
    closed: bool,
}

impl Change {
    pub fn status(&self) -> Status {
        match (&self.landing, self.closed) {
            (Some(_), _) => Status::Landed,
            (None, true) => Status::Closed,
            (None, false) => Status::Open,
        }
    }

    pub fn revision(&self, id: RevisionId) -> Option<&Revision> {
        self.revisions.iter().find(|r| r.id == id)
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
    AlreadyLanded,
    Closed,
    NotClosed,
    /// That trunk commit is already some other landing's.
    TrunkCommitTaken(CommitId),
}

impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownChange(id) => write!(f, "no change {id}"),
            Self::Duplicate => f.write_str("already recorded"),
            Self::AlreadyObserved => f.write_str("that change or PR is already in the ledger"),
            Self::UnknownRevision(id) => write!(f, "no revision {id} on that change"),
            Self::AlreadyLanded => f.write_str("the change already landed"),
            Self::Closed => f.write_str("the change is closed"),
            Self::NotClosed => f.write_str("the change isn't closed"),
            Self::TrunkCommitTaken(c) => write!(f, "trunk commit {c} already belongs to a landing"),
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
                let id = RevisionId::new(base, head);
                if self.existing(*change)?.revision(id).is_some() {
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
            } => {
                let c = self.existing(*change)?;
                if c.revision(*revision).is_none() {
                    return Err(Invalid::UnknownRevision(*revision));
                }
                let same = |e: &&Evidence| {
                    (&e.revision, &e.kind, &e.check, &e.result, &e.source)
                        == (revision, kind, check, result, source)
                };
                if c.evidence.iter().any(|e| same(&e)) {
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
                    Status::Landed => return Err(Invalid::AlreadyLanded),
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
                Status::Landed => return Err(Invalid::AlreadyLanded),
                Status::Closed => return Err(Invalid::Closed),
                Status::Open => {}
            },
            Body::Reopen { change } => {
                if self.existing(*change)?.status() != Status::Closed {
                    return Err(Invalid::NotClosed);
                }
            }
        }
        Ok(())
    }

    /// Applies an op [`Ledger::check`] has accepted.
    pub(crate) fn apply(&mut self, op: &Op) {
        let at = op.op.at;
        self.head = Some(op.id);
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
                            evidence: Vec::new(),
                            landing: None,
                            closed: false,
                        },
                    );
                }
                Body::Outside {
                    repo,
                    before,
                    after,
                } => self.outside.push(Outside {
                    repo: repo.clone(),
                    before: before.clone(),
                    after: after.clone(),
                    at,
                }),
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
            Body::Revision { base, head, .. } => c.revisions.push(Revision {
                id: RevisionId::new(base, head),
                base: base.clone(),
                head: head.clone(),
                at,
            }),
            Body::Evidence {
                revision,
                kind,
                check,
                result,
                source,
                ..
            } => c.evidence.push(Evidence {
                revision: *revision,
                kind: *kind,
                check: check.clone(),
                result: *result,
                source: source.clone(),
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
                });
            }
            Body::Close { .. } => c.closed = true,
            Body::Reopen { .. } => c.closed = false,
            Body::Observe { .. } | Body::Outside { .. } => {
                unreachable!("check refuses observing a change twice; outside names none")
            }
        }
    }
}
