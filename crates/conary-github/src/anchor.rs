//! The ledger's anchor kept off the box (`docs/DESIGN.md` section 4.6): a
//! ref on GitHub naming the ledger's key and the newest op its writer
//! published. A hash chain can't show whole ops cut from its end, or a log
//! removed and started over, so the writer checks the log against this ref
//! before it writes anything and moves the ref on after, all under the
//! ledger's lock. Each move is a commit on the one before it, so the ref's
//! history is the anchor's.
//!
//! The ref is the ledger's identity: one ledger per repository, set up once
//! by [`init`]. A writer never makes a key, so losing the ledger's directory
//! can't quietly start another ledger; it's refused until the key is back.

use std::path::Path;

use ed25519_dalek::{SigningKey, VerifyingKey};

use conary_core::Store;
use conary_core::log::{self, Anchor, Lock, LogError};
use conary_core::store::StoreError;

use crate::api::ApiError;

/// An anchor as GitHub keeps it: a commit on the ref, its message, and the
/// move before it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kept {
    pub commit: String,
    pub message: String,
    pub parent: Option<String>,
}

/// Where anchors are kept: refs in a repository.
pub trait Anchors {
    /// The commit `name` is at, if there's one.
    fn get(&self, repo: &str, name: &str) -> Result<Option<Kept>, ApiError>;
    /// One commit of a ref's history.
    fn commit(&self, repo: &str, sha: &str) -> Result<Kept, ApiError>;
    /// Moves `name` to a new commit on `parent` (making it when there's
    /// none) with `message`, only if it's still at `parent`.
    fn set(
        &self,
        repo: &str,
        name: &str,
        parent: Option<&str>,
        message: &str,
    ) -> Result<(), ApiError>;
}

#[derive(Debug)]
pub enum AnchorError {
    Api(ApiError),
    Store(StoreError),
    Log(LogError),
    /// The anchor on GitHub isn't one, is missing, or is another ledger's.
    Lost(String),
}

impl std::fmt::Display for AnchorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Api(e) => e.fmt(f),
            Self::Store(e) => e.fmt(f),
            Self::Log(e) => e.fmt(f),
            Self::Lost(why) => write!(f, "the ledger's anchor: {why}"),
        }
    }
}

impl std::error::Error for AnchorError {}

impl From<ApiError> for AnchorError {
    fn from(e: ApiError) -> Self {
        Self::Api(e)
    }
}

impl From<StoreError> for AnchorError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

impl From<LogError> for AnchorError {
    fn from(e: LogError) -> Self {
        Self::Log(e)
    }
}

/// The ref that anchors the ledger, in its anchor repository.
pub const REF: &str = "refs/conary/anchor";

/// What the anchor on GitHub says: whose ledger it is, and the newest op its
/// writer published (none for a ledger set up with no ops yet).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Remote {
    pub key: VerifyingKey,
    pub anchor: Option<Anchor>,
}

const KEY: &str = "key: ";
const ANCHOR: &str = "anchor: ";
const NO_OPS: &str = "none";

fn hex(key: &VerifyingKey) -> String {
    key.as_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

fn message(key: &VerifyingKey, anchor: Option<Anchor>) -> String {
    let (title, at) = match anchor {
        Some(a) => (
            format!("conary: the ledger's head is op {}", a.seq),
            a.to_string(),
        ),
        None => ("conary: a new ledger".to_owned(), NO_OPS.to_owned()),
    };
    format!("{title}\n\n{KEY}{}\n{ANCHOR}{at}\n", hex(key))
}

fn parse(kept: &Kept) -> Result<Remote, AnchorError> {
    let bad = || AnchorError::Lost(format!("commit {} isn't an anchor", kept.commit));
    let field = |prefix: &str| kept.message.lines().find_map(|l| l.strip_prefix(prefix));
    let key = field(KEY)
        .and_then(|k| {
            let mut bytes = [0u8; 32];
            for (i, byte) in bytes.iter_mut().enumerate() {
                *byte = u8::from_str_radix(k.get(2 * i..2 * i + 2)?, 16).ok()?;
            }
            (k.len() == 64).then_some(bytes)
        })
        .and_then(|b| VerifyingKey::from_bytes(&b).ok())
        .ok_or_else(bad)?;
    let anchor = match field(ANCHOR).ok_or_else(bad)? {
        NO_OPS => None,
        a => Some(a.parse().map_err(|()| bad())?),
    };
    Ok(Remote { key, anchor })
}

fn fetch(anchors: &dyn Anchors, repo: &str) -> Result<Option<(Kept, Remote)>, AnchorError> {
    match anchors.get(repo, REF)? {
        Some(k) => {
            let remote = parse(&k)?;
            Ok(Some((k, remote)))
        }
        None => Ok(None),
    }
}

fn not_set_up(repo: &str) -> AnchorError {
    AnchorError::Lost(format!("{repo} has no {REF}: `cn init` sets the ledger up"))
}

fn same_ledger(repo: &str, remote: &Remote, key: &VerifyingKey) -> Result<(), AnchorError> {
    if &remote.key == key {
        return Ok(());
    }
    Err(AnchorError::Lost(format!(
        "{repo} anchors the ledger signed by {}, not this one ({})",
        hex(&remote.key),
        hex(key)
    )))
}

/// Most moves the ref may be ahead of the copy beside the log for a write.
/// It's one after a write that stopped between moving the ref and keeping
/// the copy, and the next write catches the copy up; more than a few means
/// the copy is from somewhere else, and `cn init` with it removed checks the
/// ref's whole history instead.
const MAX_AHEAD: usize = 16;

/// The checkpoints the log must hold besides `tip`'s: every move on the ref
/// since the one the copy beside the log names (`local`, with `None` for no
/// copy, and `Some(None)` for a copy kept before the ledger had ops). The
/// writer keeps that copy only once the ref has moved and the log was checked
/// against it. With no copy, it's every move back to the one that set the
/// ledger up. Each must name this ledger's key and come before the one after
/// it, down to and including the move before the copy's, so a ref moved
/// back, even to a move naming the copy's op again, or a longer history in
/// place of one the log acknowledged, is refused; checking each against the
/// log refuses a log rewritten past one of them.
fn checkpoints(
    anchors: &dyn Anchors,
    repo: &str,
    tip: &Kept,
    remote: &Remote,
    local: Option<Option<Anchor>>,
) -> Result<Vec<Anchor>, AnchorError> {
    let lost = |what: String| {
        let checked = match local {
            None => "never".to_owned(),
            Some(None) => "with no ops".to_owned(),
            Some(Some(a)) => format!("at op {a}"),
        };
        AnchorError::Lost(format!(
            "{repo}'s {REF} {what}, and the log was last checked {checked}"
        ))
    };
    let named = |a: Option<Anchor>| a.map_or_else(|| "no op".to_owned(), |a| format!("op {a}"));
    let mut held = Vec::new();
    let mut at = tip.clone();
    let mut newer = None;
    let mut reached = false;
    loop {
        let r = parse(&at)?;
        same_ledger(repo, &r, &remote.key)?;
        let seq = r.anchor.map_or(0, |a| a.seq);
        if newer.is_some_and(|n| seq >= n) {
            return Err(lost(format!("went back at commit {}", at.commit)));
        }
        if reached {
            return Ok(held);
        }
        reached = local == Some(r.anchor);
        if let Some(l) = local.filter(|l| !reached && seq <= l.map_or(0, |a| a.seq)) {
            return Err(lost(format!(
                "names {} by commit {}, never {}",
                named(r.anchor),
                at.commit,
                named(l)
            )));
        }
        if newer.is_some() {
            held.extend(r.anchor);
        }
        newer = Some(seq);
        let Some(parent) = at.parent.clone() else {
            return match r.anchor {
                None => Ok(held),
                Some(_) => Err(lost(format!(
                    "starts at commit {}, not a new ledger",
                    at.commit
                ))),
            };
        };
        if local.is_some() && held.len() > MAX_AHEAD {
            return Err(lost(format!("is more than {MAX_AHEAD} moves ahead")));
        }
        at = anchors.commit(repo, &parent)?;
    }
}

/// Reads the log at `dir`, checked against the ref's `tip` and the
/// `checkpoints` before it.
fn read_holding(
    dir: &Path,
    key: &VerifyingKey,
    tip: Option<Anchor>,
    checkpoints: &[Anchor],
) -> Result<(), AnchorError> {
    let ops = log::read(dir, key, tip)?;
    for a in checkpoints {
        a.check(&ops)?;
    }
    Ok(())
}

/// The copy beside the log, when there's one: a reader's, or a set-up's
/// that may be finishing one that stopped partway.
fn kept(dir: &Path) -> Result<Option<Option<Anchor>>, AnchorError> {
    Ok(if log::has_anchor(dir) {
        Some(log::load_anchor(dir)?)
    } else {
        None
    })
}

/// Sets up the ledger at `dir`, its key at `key_path`, anchored in `repo`,
/// and returns its anchor. Run again, it finishes a set-up that stopped
/// partway and changes nothing on one that's done. With an anchor already in
/// `repo`, it only takes the key that anchor names: a ledger whose key was
/// lost needs it back, never a new one. With none there, a log that already
/// has ops is refused, since it lost its anchor.
pub fn init(
    dir: &Path,
    key_path: &Path,
    anchors: &dyn Anchors,
    repo: &str,
) -> Result<Remote, AnchorError> {
    let lock = Lock::take(dir)?;
    let remote = match fetch(anchors, repo)? {
        Some((tip, remote)) => {
            let key = log::load_signing_key(key_path).map_err(|e| match e {
                LogError::NoKey(path) => AnchorError::Lost(format!(
                    "{repo} anchors the ledger signed by {}; put its key back at {}",
                    hex(&remote.key),
                    path.display()
                )),
                e => e.into(),
            })?;
            same_ledger(repo, &remote, &key.verifying_key())?;
            let held = checkpoints(anchors, repo, &tip, &remote, kept(dir)?)?;
            read_holding(dir, &remote.key, remote.anchor, &held)?;
            remote
        }
        None => {
            let key = log::load_or_create_key(key_path)?.verifying_key();
            if !log::read(dir, &key, None)?.is_empty() {
                return Err(AnchorError::Lost(format!(
                    "{repo} has no {REF}, but the log has ops"
                )));
            }
            anchors.set(repo, REF, None, &message(&key, None))?;
            Remote { key, anchor: None }
        }
    };
    log::save_anchor(&lock, remote.anchor)?;
    Ok(remote)
}

/// The ledger, open for writing, once the anchor on GitHub is found to be
/// its own, every move on it since the copy beside the log goes forward, and
/// the log holds each of them. The anchor is read under the ledger's lock and
/// the log isn't touched until it's checked against it.
pub struct Anchored {
    pub store: Store,
    repo: String,
    kept: Kept,
    remote: Remote,
}

pub fn open(
    dir: &Path,
    key: SigningKey,
    anchors: &dyn Anchors,
    repo: &str,
) -> Result<Anchored, AnchorError> {
    let lock = Lock::take(dir)?;
    let (kept, remote) = fetch(anchors, repo)?.ok_or_else(|| not_set_up(repo))?;
    same_ledger(repo, &remote, &key.verifying_key())?;
    let local = log::load_anchor(dir)?;
    let held = checkpoints(anchors, repo, &kept, &remote, Some(local))?;
    // The store checks the tip's when it opens.
    if !held.is_empty() {
        read_holding(dir, &remote.key, None, &held)?;
    }
    let store = Store::open(lock, key, remote.anchor)?;
    Ok(Anchored {
        store,
        repo: repo.to_owned(),
        kept,
        remote,
    })
}

impl Anchored {
    /// Moves the anchor on GitHub to the log's head, and keeps a copy beside
    /// the log for readers. Nothing to move when nothing was written.
    pub fn publish(&self, anchors: &dyn Anchors) -> Result<(), AnchorError> {
        let head = self.store.head();
        if head != self.remote.anchor {
            let msg = message(&self.remote.key, head);
            anchors.set(&self.repo, REF, Some(&self.kept.commit), &msg)?;
        }
        log::save_anchor(self.store.lock(), head)?;
        Ok(())
    }
}

/// Checks the log at `dir` against the anchor on GitHub: the remote check,
/// on demand, for anyone who can read the log. The copy beside the log is
/// read first, then the anchor, then the log, the order a writer moves them
/// in reverse, so a writer at work can only have moved each one on since
/// the one before it was read.
pub fn verify(
    dir: &Path,
    key: &VerifyingKey,
    anchors: &dyn Anchors,
    repo: &str,
) -> Result<Option<Anchor>, AnchorError> {
    let local = kept(dir)?;
    let (tip, remote) = fetch(anchors, repo)?.ok_or_else(|| not_set_up(repo))?;
    same_ledger(repo, &remote, key)?;
    let held = checkpoints(anchors, repo, &tip, &remote, local)?;
    read_holding(dir, key, remote.anchor, &held)?;
    Ok(remote.anchor)
}

#[cfg(test)]
mod tests;
