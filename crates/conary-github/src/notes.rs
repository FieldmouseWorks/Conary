//! Writing the ledger's notes (`docs/notes.md`): each landed change's note
//! on the trunk commit its landing made, on `refs/notes/conary` in the
//! change's own repository, so a clone that can't reach the ledger can still
//! read it. A note is derived from the ledger, so writing them again is
//! always safe: only notes that are missing or say something else are
//! written, in one commit on the ref.

use std::collections::HashMap;

use sha1::{Digest, Sha1};

use conary_core::Ledger;
use conary_core::notes::{Links, Note};
use conary_core::op::Repo;

use crate::api::ApiError;

/// The notes ref as GitHub keeps it: its commit and tree, and each note's
/// path and blob, by the commit the note is on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Kept {
    pub commit: String,
    pub tree: String,
    pub notes: HashMap<String, (String, String)>,
}

/// Where notes are kept: a notes ref in a repository.
pub trait Notes {
    /// The ref `name`, if there's one.
    fn get(&self, repo: &str, name: &str) -> Result<Option<Kept>, ApiError>;
    /// Moves `name` to a new commit on `parent` (making it when there's
    /// none) whose tree is `parent`'s with `files` (path, text) written over
    /// it, only if the ref is still at `parent`.
    fn set(
        &self,
        repo: &str,
        name: &str,
        parent: Option<&Kept>,
        files: &[(String, String)],
    ) -> Result<(), ApiError>;
}

/// The id git gives a blob of `text`.
pub fn blob_id(text: &str) -> String {
    let mut hash = Sha1::new();
    hash.update(format!("blob {}\0", text.len()));
    hash.update(text);
    hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Brings `repo`'s notes up to date with `ledger`, and returns how many
/// notes were written.
pub fn publish(
    notes: &dyn Notes,
    ledger: &Ledger,
    repo: &Repo,
    links: Links,
) -> Result<usize, ApiError> {
    let full = repo.to_string();
    let kept = notes.get(&full, conary_core::notes::REF)?;
    let mut files = Vec::new();
    for c in ledger.changes().filter(|c| &c.repo == repo) {
        let Some(landing) = &c.landing else {
            continue;
        };
        let text = Note::of(c, landing, links).text();
        let on = landing.after.to_string();
        // A note already there is written over where it is: git keeps a
        // ref with many notes in a fan-out of directories.
        match kept.as_ref().and_then(|k| k.notes.get(&on)) {
            Some((_, blob)) if *blob == blob_id(&text) => {}
            Some((path, _)) => files.push((path.clone(), text)),
            None => files.push((on, text)),
        }
    }
    if !files.is_empty() {
        notes.set(&full, conary_core::notes::REF, kept.as_ref(), &files)?;
    }
    Ok(files.len())
}

#[cfg(test)]
mod tests;
