//! Opening a [`Store`] without verifying or replaying its prefix under the
//! writer lock: [`Store::prepare`] reads, verifies and replays complete lines
//! before the lock is taken, and [`Store::open_prepared`] catches up only what
//! was appended since.

use std::collections::HashSet;
use std::path::Path;

use ed25519_dalek::SigningKey;

use crate::ledger::Ledger;
use crate::log::{self, Anchor, Lock, LogError, OpLog};

use super::{Store, StoreError};

/// The log verified and replayed up to its last complete line before taking
/// the writer lock.
pub struct Prepared {
    log: log::Prepared,
    ledger: Ledger,
}

/// The ledger verified under its [`Lock`], before the log's mark is updated or
/// its tail is mended. Its caller can check any anchors and save its anchor
/// copy under the same lock, then [`Opening::finish`] commits, lets the lock
/// go and applies any tail ops appended since [`Store::prepare`].
pub struct Opening {
    log: log::Opening,
    ledger: Option<Ledger>,
}

impl Opening {
    /// The writer lock held while opening.
    pub fn lock(&self) -> &Lock {
        self.log.lock()
    }

    /// Whether the verified log holds `anchor`.
    pub fn check(&self, anchor: Anchor) -> Result<(), LogError> {
        self.log.check(anchor)
    }

    /// Commits the open log, lets the writer lock go, and returns the store
    /// with any tail ops replayed.
    pub fn finish(self) -> Result<Store, StoreError> {
        let (log, ops) = self.log.finish()?;
        let ledger = match self.ledger {
            Some(mut ledger) => {
                for op in &ops {
                    ledger
                        .check(&op.op.body)
                        .map_err(|e| StoreError::Replay(op.id, e))?;
                    ledger.apply(op);
                }
                ledger
            }
            None => Ledger::replay(&ops).map_err(|(id, e)| StoreError::Replay(id, e))?,
        };
        Ok(Store {
            log,
            ledger,
            broken: None,
            overtaken: HashSet::new(),
        })
    }
}

impl Store {
    /// Reads, verifies and replays every complete line currently in the log at
    /// `dir` without taking the writer lock.
    pub fn prepare(dir: &Path, key: SigningKey) -> Result<Prepared, StoreError> {
        let mut log = OpLog::prepare(dir, key)?;
        let ops = log.take_ops();
        let ledger = Ledger::replay(&ops).map_err(|(id, e)| StoreError::Replay(id, e))?;
        Ok(Prepared { log, ledger })
    }

    /// Opens the ledger in `lock`'s directory under `lock`, reusing `prepared`
    /// when the log hasn't changed behind its prepared offset. Nothing on disk
    /// changes until [`Opening::finish`].
    pub fn open_prepared(
        prepared: Option<Prepared>,
        lock: Lock,
        key: SigningKey,
    ) -> Result<Opening, StoreError> {
        let (log_prepared, ledger) = match prepared {
            Some(p) => (Some(p.log), Some(p.ledger)),
            None => (None, None),
        };
        let log = OpLog::open_prepared(log_prepared, lock, key)?;
        let ledger = log.reused_prefix().then_some(ledger).flatten();
        Ok(Opening { log, ledger })
    }
}
