//! The ledger's anchor kept off the box (`docs/DESIGN.md` section 4.6): a
//! ref on GitHub naming the newest op its writer published. A hash chain
//! can't show whole ops cut from its end, or a log removed and started over,
//! so the writer checks the log against this ref before it writes anything
//! and moves the ref on after. Each move is a commit on the one before it, so
//! the ref's history is the anchor's.

use ed25519_dalek::{SigningKey, VerifyingKey};

use conary_core::Store;
use conary_core::log::{self, Anchor, LogError};
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
    /// The anchor on GitHub isn't one, or is gone while the log has ops.
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

/// The ref a ledger signed by `key` is anchored at: one per key, so ledgers
/// on several machines can share a repository.
pub fn ref_name(key: &VerifyingKey) -> String {
    let hex: String = key.as_bytes()[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("refs/conary/anchor/{hex}")
}

const PREFIX: &str = "anchor: ";

fn message(anchor: Anchor) -> String {
    format!(
        "conary: the ledger's head is op {}\n\n{PREFIX}{anchor}\n",
        anchor.seq
    )
}

fn parse(kept: &Kept) -> Result<Anchor, AnchorError> {
    kept.message
        .lines()
        .find_map(|l| l.strip_prefix(PREFIX))
        .and_then(|a| a.parse().ok())
        .ok_or_else(|| AnchorError::Lost(format!("commit {} names no op", kept.commit)))
}

/// The ledger, open for writing, once its log is found to hold the anchor on
/// GitHub. With no anchor there, only a log with no ops yet is taken: one
/// that has ops lost its anchor, and starting it over would drop them.
pub struct Anchored {
    pub store: Store,
    repo: String,
    name: String,
    kept: Option<(Kept, Anchor)>,
}

pub fn open(
    dir: &std::path::Path,
    key: SigningKey,
    anchors: &dyn Anchors,
    repo: &str,
) -> Result<Anchored, AnchorError> {
    let name = ref_name(&key.verifying_key());
    let kept = match anchors.get(repo, &name)? {
        Some(k) => {
            let a = parse(&k)?;
            Some((k, a))
        }
        None => None,
    };
    let store = Store::open(dir, key, kept.as_ref().map(|(_, a)| *a))?;
    if kept.is_none() && store.head().is_some() {
        return Err(AnchorError::Lost(format!(
            "{repo} has no {name}, but the log has ops"
        )));
    }
    Ok(Anchored {
        store,
        repo: repo.to_owned(),
        name,
        kept,
    })
}

impl Anchored {
    /// Moves the anchor on GitHub to the log's head, and keeps a copy beside
    /// the log for readers. Nothing to move when nothing was written.
    pub fn publish(&self, anchors: &dyn Anchors, dir: &std::path::Path) -> Result<(), AnchorError> {
        let Some(head) = self.store.head() else {
            return Ok(());
        };
        if self.kept.as_ref().map(|(_, a)| *a) != Some(head) {
            let parent = self.kept.as_ref().map(|(k, _)| k.commit.as_str());
            anchors.set(&self.repo, &self.name, parent, &message(head))?;
        }
        log::save_anchor(dir, head)?;
        Ok(())
    }
}

/// Checks the log at `dir` against the anchor on GitHub and keeps that
/// anchor beside it for readers: the remote check, on demand.
pub fn verify(
    dir: &std::path::Path,
    key: &VerifyingKey,
    anchors: &dyn Anchors,
    repo: &str,
) -> Result<Option<Anchor>, AnchorError> {
    let name = ref_name(key);
    let anchor = anchors.get(repo, &name)?.map(|k| parse(&k)).transpose()?;
    let ops = log::read(dir, key, anchor)?;
    match anchor {
        Some(a) => log::save_anchor(dir, a)?,
        None if !ops.is_empty() => {
            return Err(AnchorError::Lost(format!(
                "{repo} has no {name}, but the log has ops"
            )));
        }
        None => {}
    }
    Ok(anchor)
}

#[cfg(test)]
mod tests;
