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

/// An anchor as GitHub keeps it: the commit the ref names and its message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kept {
    pub commit: String,
    pub message: String,
}

/// Where anchors are kept: refs in a repository.
pub trait Anchors {
    fn get(&self, repo: &str, name: &str) -> Result<Option<Kept>, ApiError>;
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
        Some((_, remote)) => {
            let key = log::load_signing_key(key_path).map_err(|e| match e {
                LogError::NoKey(path) => AnchorError::Lost(format!(
                    "{repo} anchors the ledger signed by {}; put its key back at {}",
                    hex(&remote.key),
                    path.display()
                )),
                e => e.into(),
            })?;
            same_ledger(repo, &remote, &key.verifying_key())?;
            log::read(dir, &remote.key, remote.anchor)?;
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
/// its own and its log holds it. The anchor is read under the ledger's lock
/// and the log isn't touched until it's checked against it.
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
/// on demand, for anyone who can read the log. The anchor is read first, so
/// a writer at work can only have added to the log since.
pub fn verify(
    dir: &Path,
    key: &VerifyingKey,
    anchors: &dyn Anchors,
    repo: &str,
) -> Result<Option<Anchor>, AnchorError> {
    let (_, remote) = fetch(anchors, repo)?.ok_or_else(|| not_set_up(repo))?;
    same_ledger(repo, &remote, key)?;
    log::read(dir, key, remote.anchor)?;
    Ok(remote.anchor)
}

#[cfg(test)]
mod tests;
