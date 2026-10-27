//! The ledger open for writing: the op log and the ledger it replays to,
//! kept in step. Every write is checked against the ledger before it's
//! appended, so the log only ever holds ops that apply.

use std::time::{SystemTime, UNIX_EPOCH};

use ed25519_dalek::SigningKey;

use crate::id::OpId;
use crate::ledger::{Invalid, Ledger};
use crate::log::{Anchor, Lock, LogError, OpLog};
use crate::op::Body;

#[derive(Debug)]
pub enum StoreError {
    Log(LogError),
    /// The op can't be applied; nothing was written.
    Invalid(Invalid),
    /// The log verifies but holds an op that doesn't apply.
    Replay(OpId, Invalid),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Log(e) => e.fmt(f),
            Self::Invalid(e) => e.fmt(f),
            Self::Replay(id, e) => write!(f, "op {id} in the log doesn't apply: {e}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<LogError> for StoreError {
    fn from(e: LogError) -> Self {
        Self::Log(e)
    }
}

pub struct Store {
    log: OpLog,
    ledger: Ledger,
}

impl Store {
    /// Opens the ledger in `lock`'s directory for writing, refusing a log
    /// that doesn't hold `anchor`, the newest one its caller holds (and read
    /// under that lock, so no other writer moved it since).
    pub fn open(lock: Lock, key: SigningKey, anchor: Option<Anchor>) -> Result<Self, StoreError> {
        let (log, ops) = OpLog::open(lock, key, anchor)?;
        let ledger = Ledger::replay(&ops).map_err(|(id, e)| StoreError::Replay(id, e))?;
        Ok(Self { log, ledger })
    }

    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    /// The last op recorded, to anchor the log at.
    pub fn head(&self) -> Option<Anchor> {
        self.log.head()
    }

    /// The writer lock this store holds.
    pub fn lock(&self) -> &Lock {
        self.log.lock()
    }

    /// Records one op, now.
    pub fn record(&mut self, body: Body) -> Result<OpId, StoreError> {
        self.record_at(body, now())
    }

    pub fn record_at(&mut self, body: Body, at: i64) -> Result<OpId, StoreError> {
        self.ledger.check(&body).map_err(StoreError::Invalid)?;
        let op = self.log.append(body, at)?;
        self.ledger.apply(&op);
        Ok(op.id)
    }
}

/// Milliseconds since the Unix epoch.
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{ChangeId, CommitId, RevisionId};
    use crate::ledger::Status;
    use crate::log;
    use crate::op::{Actor, ActorKind, CheckKind, Outcome, Repo};

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[3; 32])
    }

    fn repo() -> Repo {
        Repo {
            owner: "FieldmouseWorks".into(),
            name: "burrow".into(),
        }
    }

    fn commit(c: char) -> CommitId {
        c.to_string().repeat(40).parse().unwrap()
    }

    fn observe(store: &mut Store, pr: u64) -> ChangeId {
        let change = ChangeId::new().unwrap();
        store
            .record(Body::Observe {
                change,
                repo: repo(),
                pr,
                author: Actor {
                    kind: ActorKind::Agent,
                    login: "TusanHomichi".into(),
                    session: Some("https://claude.ai/code/session_x".into()),
                },
            })
            .unwrap();
        change
    }

    #[test]
    fn a_change_is_seen_revised_checked_and_landed() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(Lock::take(dir.path()).unwrap(), key(), None).unwrap();
        let change = observe(&mut store, 9);
        store
            .record(Body::Revision {
                change,
                base: commit('a'),
                head: commit('b'),
            })
            .unwrap();
        let revision = RevisionId::new(&commit('a'), &commit('b'));
        store
            .record(Body::Evidence {
                change,
                revision,
                kind: CheckKind::Ci,
                check: "check / check".into(),
                result: Outcome::Pass,
                source: "https://github.com/x/y/actions/runs/1".into(),
            })
            .unwrap();
        store
            .record(Body::Land {
                change,
                revision,
                before: commit('a'),
                after: commit('c'),
            })
            .unwrap();
        drop(store);

        let ops = log::read(dir.path(), &key().verifying_key(), None).unwrap();
        let ledger = Ledger::replay(&ops).unwrap();
        let landed = ledger.landed_as(&repo(), &commit('c')).unwrap();
        assert_eq!(landed.id, change);
        assert_eq!(landed.pr, 9);
        assert_eq!(landed.status(), Status::Landed);
        assert_eq!(landed.evidence.len(), 1);
        assert_eq!(ledger.by_pr(&repo(), 9).unwrap().id, change);
        // GitHub ignores case in a repository's name, and so does the ledger.
        let spelled = Repo {
            owner: "fieldmouseworks".into(),
            name: "Burrow".into(),
        };
        assert_eq!(ledger.landed_as(&spelled, &commit('c')).unwrap().id, change);
    }

    #[test]
    fn ops_that_dont_apply_are_refused_and_never_written() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(Lock::take(dir.path()).unwrap(), key(), None).unwrap();
        let change = observe(&mut store, 1);
        let unknown = RevisionId::new(&commit('a'), &commit('d'));
        let refused = [
            Body::Revision {
                change: ChangeId::new().unwrap(),
                base: commit('a'),
                head: commit('b'),
            },
            Body::Land {
                change,
                revision: unknown,
                before: commit('a'),
                after: commit('c'),
            },
            Body::Reopen { change },
            Body::Observe {
                change: ChangeId::new().unwrap(),
                repo: repo(),
                pr: 1,
                author: store.ledger().change(change).unwrap().author.clone(),
            },
        ];
        for body in refused {
            assert!(matches!(store.record(body), Err(StoreError::Invalid(_))));
        }
        let revision = Body::Revision {
            change,
            base: commit('a'),
            head: commit('b'),
        };
        store.record(revision.clone()).unwrap();
        assert!(matches!(
            store.record(revision),
            Err(StoreError::Invalid(Invalid::Duplicate))
        ));
        store.record(Body::Close { change }).unwrap();
        let revision = RevisionId::new(&commit('a'), &commit('b'));
        let land = Body::Land {
            change,
            revision,
            before: commit('a'),
            after: commit('c'),
        };
        assert!(matches!(
            store.record(land),
            Err(StoreError::Invalid(Invalid::Closed))
        ));
        drop(store);
        assert_eq!(
            log::read(dir.path(), &key().verifying_key(), None)
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn changes_are_listed_in_the_order_they_were_seen() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(Lock::take(dir.path()).unwrap(), key(), None).unwrap();
        let seen: Vec<ChangeId> = (1..=20).map(|pr| observe(&mut store, pr)).collect();
        let listed: Vec<ChangeId> = store.ledger().changes().map(|c| c.id).collect();
        assert_eq!(listed, seen);
    }

    #[test]
    fn two_changes_cant_claim_one_trunk_commit() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(Lock::take(dir.path()).unwrap(), key(), None).unwrap();
        for pr in [1, 2] {
            let change = observe(&mut store, pr);
            store
                .record(Body::Revision {
                    change,
                    base: commit('a'),
                    head: commit('b'),
                })
                .unwrap();
            let land = store.record(Body::Land {
                change,
                revision: RevisionId::new(&commit('a'), &commit('b')),
                before: commit('a'),
                after: commit('c'),
            });
            assert_eq!(land.is_ok(), pr == 1);
        }
    }

    #[test]
    fn trunk_can_move_to_the_same_commit_more_than_once() {
        // Trunk at c, moved away, then reset to c, and reset to a landed
        // commit: each is a movement of its own.
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(Lock::take(dir.path()).unwrap(), key(), None).unwrap();
        let change = observe(&mut store, 1);
        store
            .record(Body::Revision {
                change,
                base: commit('a'),
                head: commit('b'),
            })
            .unwrap();
        store
            .record(Body::Land {
                change,
                revision: RevisionId::new(&commit('a'), &commit('b')),
                before: commit('a'),
                after: commit('d'),
            })
            .unwrap();
        for after in ['c', 'e', 'c', 'd'] {
            store
                .record(Body::Outside {
                    repo: repo(),
                    before: None,
                    after: commit(after),
                })
                .unwrap();
        }
        assert_eq!(store.ledger().outside().len(), 4);
    }

    #[test]
    fn a_store_whose_log_lost_its_anchored_op_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(Lock::take(dir.path()).unwrap(), key(), None).unwrap();
        observe(&mut store, 1);
        let anchor = store.head().unwrap();
        drop(store);
        Store::open(Lock::take(dir.path()).unwrap(), key(), Some(anchor)).unwrap();
        std::fs::remove_file(dir.path().join("ops.jsonl")).unwrap();
        assert!(matches!(
            Store::open(Lock::take(dir.path()).unwrap(), key(), Some(anchor)),
            Err(StoreError::Log(LogError::Unanchored(_)))
        ));
    }
}
