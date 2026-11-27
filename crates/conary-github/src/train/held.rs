//! Every operation of a train holds its repository's train lock throughout,
//! wherever it's called, a flow pass included: two steps, joins, departures
//! or drops of the same repository never overlap (`docs/DESIGN.md`, R2).
//! A step takes the ledger's writer lock only for each write (or batch), so
//! its GitHub reads never hold up other repositories' writers. Joins,
//! departures and drops take the repository's train lock first and then the
//! ledger's writer lock (never the reverse), keeping the lock order strict.

use std::cell::RefCell;
use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::*;

thread_local! {
    /// Per-repository train locks currently held by this thread, keyed by
    /// their canonicalized lock file path so nested calls on the same thread
    /// don't self-deadlock on `flock`.
    static HELD: RefCell<HashSet<PathBuf>> = RefCell::new(HashSet::new());
}

/// The lock file for `repo`'s train under the ledger directory `dir`.
/// Case-insensitive on owner and name, matching [`Repo`].
pub(crate) fn lock_path(dir: &Path, repo: &Repo) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(repo.owner.to_ascii_lowercase().as_bytes());
    hasher.update(b"\0");
    hasher.update(repo.name.to_ascii_lowercase().as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for b in digest {
        let _ = write!(hex, "{b:02x}");
    }
    dir.join("train").join(format!("{hex}.lock"))
}

struct RepoLock {
    file: File,
    key: PathBuf,
}

impl Drop for RepoLock {
    /// Unlocks explicitly before closing so a child forked on another thread
    /// doesn't keep the flock alive until exec.
    fn drop(&mut self) {
        let _ = self.file.unlock();
        HELD.with(|held| {
            held.borrow_mut().remove(&self.key);
        });
    }
}

/// Runs `then` while holding `repo`'s per-repository train lock under
/// `store.dir()`. Reentrant on the same thread: an outer call keeps the lock
/// until it returns.
pub(crate) fn holding_repo<T>(
    store: &mut Store,
    repo: &Repo,
    then: impl FnOnce(&mut Store) -> T,
) -> Result<T, io::Error> {
    let path = lock_path(store.dir(), repo);
    let parent = path.parent().expect("train lock has parent dir");
    fs::create_dir_all(parent)?;
    let key = parent
        .canonicalize()
        .unwrap_or_else(|_| parent.to_owned())
        .join(path.file_name().expect("train lock has file name"));
    if HELD.with(|held| held.borrow().contains(&key)) {
        return Ok(then(store));
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)?;
    file.lock()?;
    HELD.with(|held| {
        held.borrow_mut().insert(key.clone());
    });
    let _guard = RepoLock { file, key };
    Ok(then(store))
}

/// Puts a PR in its repo's train at `head`, the head everything but the
/// gate said can land, once the ledger holds the PR as GitHub shows it now
/// and that is its head. Its class is the policy's ([`class::of`], which
/// records it): an R2 change rides alone, in a candidate of its own, which
/// lands only while trunk is still its base. With `negatives`, only once
/// its revision's negative counts ([`Negatives`]).
/// `settled` is the raise labels the steward settled it with, as `cn class`
/// told it: it joins only when they're every raise its change has once
/// it's classed with its labels now, since a raise the steward didn't
/// settle it with, recorded by anyone, would land on a settlement made
/// without it ([`Joined::Raised`]).
/// Its head's train status goes to pending first, so a success left from an
/// earlier time in the train can't let anything merge it.
#[allow(clippy::too_many_arguments)]
pub fn join(
    store: &mut Store,
    forge: &dyn Forge,
    source: &dyn Source,
    config: &Config,
    repo: &Repo,
    pr: u64,
    head: &str,
    settled: &[String],
    policy: &Policy,
    negatives: Option<&Negatives>,
) -> Result<Joined, TrainError> {
    holding_repo(store, repo, |store| {
        store.holding(|store| {
            held_join(
                store, forge, source, config, repo, pr, head, settled, policy, negatives,
            )
        })
    })
    .map_err(TrainError::Lock)??
}

/// Takes a PR out of its repo's train, its head's train status pending
/// first. Not while a landing is open, its head saying it's landing with
/// the candidate it's next in, its merge maybe on trunk, not yet shown: then every
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
    holding_repo(store, repo, |store| {
        store.holding(|store| held_leave(store, forge, source, repo, pr, why))
    })
    .map_err(TrainError::Lock)??
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
    holding_repo(store, repo, |store| {
        store.holding(|store| held_drop_candidate(store, forge, source, repo, candidate))
    })
    .map_err(TrainError::Lock)??
}

/// One step of a repo's train, the whole of what it does on an event. It
/// finishes the candidates whose landings all came through, builds the next
/// candidate when none is in flight, reads the gate of the one gating and
/// says its verdict once its checks have (a failure's, after its second
/// try), lands a green one's changes in order, lets a change red alone whose
/// gate passed since join again, and deletes the branches of every candidate that isn't in flight.
/// It goes on until the train waits on something: a gate still running, a
/// merge GitHub hasn't shown yet, or nothing to do. It holds its repository's
/// train lock throughout, so two steps of the same repository never overlap
/// while its GitHub reads hold no ledger-wide lock and its writes take the
/// ledger's writer lock per op or batch. Trunk is the repo's default branch
/// as GitHub says it is when the step starts: the branch join, landing and
/// ingest all mean, never a setting of its own. A step with no check required
/// is refused before it does anything.
pub fn step(
    store: &mut Store,
    forge: &dyn Forge,
    source: &dyn Source,
    config: &Config,
    repo: &Repo,
    settings: &Settings,
) -> Result<Stepped, TrainError> {
    step_before_land(store, forge, source, config, repo, settings, &mut |_, _| {
        Ok(true)
    })
}

/// [`step`], calling `before_land` right before each PR is marked landing and
/// merged (after the last ledger write ahead of the merge). Returning `false`
/// stops the step without marking or merging that PR.
pub(crate) fn step_before_land(
    store: &mut Store,
    forge: &dyn Forge,
    source: &dyn Source,
    config: &Config,
    repo: &Repo,
    settings: &Settings,
    before_land: &mut dyn FnMut(&mut Store, u64) -> Result<bool, TrainError>,
) -> Result<Stepped, TrainError> {
    if settings.require.is_empty() {
        return Err(TrainError::NothingRequired);
    }
    holding_repo(store, repo, |store| {
        held_step(store, forge, source, config, repo, settings, before_land)
    })
    .map_err(TrainError::Lock)?
}
