//! A sync of several repos on one open of the ledger, what `cn sync` runs:
//! the log is read, verified and replayed once, and each repo after that
//! costs only the ops other writers appended since.
//!
//! Each repo in turn is read from GitHub ([`gather`]) with the ledger's
//! lock let go. Then the lock is taken again, what other writers appended
//! is caught up on, and the anchor's ref is read and checked before
//! anything is written ([`Anchored::hold`]). Under that lock the repo is
//! synced ([`sync`]), the anchor is moved on to what it recorded and its
//! notes are written, and the lock is let go before the next repo's gather.
//! A repo's own failure leaves the rest to sync; one of the ledger's or its
//! anchor's, after which writing on isn't safe, stops the pass.

use std::path::Path;

use ed25519_dalek::SigningKey;

use conary_core::notes::Links;
use conary_core::op::Repo;
use conary_core::store::StoreError;

use super::{Config, IngestError, Synced, gather, sync};
use crate::anchor::{self, AnchorError, Anchored, Anchors};
use crate::api::{Source, unavailable};
use crate::notes::{self, Notes};

/// A repo to sync, and whether its notes keep session links.
#[derive(Debug, Clone)]
pub struct Target {
    pub repo: Repo,
    pub links: Links,
}

/// How a repo's sync ended.
#[derive(Debug)]
pub enum Ended {
    /// Recorded, anchored, and its notes written. The undo PRs it refused
    /// are in it, each refused alone.
    Done(Synced),
    /// GitHub couldn't be asked just now: syncing it again later is the
    /// whole fix. What it recorded before is anchored.
    Unavailable(String),
    /// Anything else. What it recorded before is anchored.
    Failed(String),
}

impl Ended {
    fn of(e: &(dyn std::error::Error + 'static)) -> Self {
        if unavailable(e) {
            Self::Unavailable(e.to_string())
        } else {
            Self::Failed(e.to_string())
        }
    }
}

/// A pass that stopped because the ledger or its anchor failed, so writing
/// on isn't safe. What every repo before `unsynced` recorded is anchored.
#[derive(Debug)]
pub struct Stopped {
    pub error: AnchorError,
    /// The repos it didn't sync: the one it stopped at, and every one after
    /// it.
    pub unsynced: Vec<Repo>,
}

impl std::fmt::Display for Stopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let unsynced: Vec<String> = self.unsynced.iter().map(Repo::to_string).collect();
        write!(f, "{}; not synced: {}", self.error, unsynced.join(", "))
    }
}

impl std::error::Error for Stopped {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Syncs each of `targets` in turn, the ledger at `dir` opened once, its
/// anchor in `anchor_repo`, and tells `done` how each ended as it ends,
/// after its lock is let go. The ledger is opened only once a repo is
/// gathered, so a pass whose every gather fails never touches it. A repo
/// whose gather, sync or notes fail ends [`Ended::Unavailable`] or
/// [`Ended::Failed`], and the next is synced all the same; what a failed
/// sync recorded is anchored and noted as ever. A failure to open the
/// ledger, take its lock again, check or move its anchor, or of its log
/// during a sync, stops the pass: [`Stopped`] names the repos not synced.
pub fn sync_all<G: Source + Anchors + Notes>(
    dir: &Path,
    key: &SigningKey,
    anchor_repo: &str,
    github: &G,
    config: &Config,
    targets: &[Target],
    mut done: impl FnMut(&Repo, Ended),
) -> Result<(), Stopped> {
    let mut anchored: Option<Anchored> = None;
    for (i, Target { repo, links }) in targets.iter().enumerate() {
        let stop = |error: AnchorError| Stopped {
            error,
            unsynced: targets[i..].iter().map(|t| t.repo.clone()).collect(),
        };
        let gathered = match gather(github, config, repo, dir) {
            Ok(gathered) => gathered,
            Err(e) => {
                done(repo, Ended::of(&e));
                continue;
            }
        };
        let a = match &mut anchored {
            Some(a) => a,
            None => {
                anchored.insert(anchor::open(dir, key.clone(), github, anchor_repo).map_err(stop)?)
            }
        };
        a.hold(github).map_err(stop)?;
        let got = sync(&mut a.store, github, gathered);
        // What it recorded before a failure is anchored all the same.
        let published = a.publish(github);
        let got = match got {
            // The log or the ledger it replays to can't be trusted with
            // another write. An op refused before it was written leaves
            // the store as it was: that's the repo's.
            Err(IngestError::Store(e)) if !matches!(e, StoreError::Invalid(_)) => {
                return Err(stop(AnchorError::Store(e)));
            }
            got => got,
        };
        published.map_err(stop)?;
        let noted = notes::publish(github, a.store.ledger(), repo, *links);
        a.release();
        let outcome = match (got, noted) {
            (Ok(synced), Ok(_)) => Ended::Done(synced),
            (Err(e), _) => Ended::of(&e),
            (Ok(_), Err(e)) => Ended::of(&e),
        };
        done(repo, outcome);
    }
    Ok(())
}
