//! `why`: the change that landed a line (`docs/DESIGN.md` section 7), from
//! the ledger, or from the notes on the checkout's origin where the ledger
//! can't be reached.

use std::collections::HashSet;
use std::path::Path;

use serde::Serialize;

use crate::git::{self, GitError};
use crate::id::CommitId;
use crate::ledger::Ledger;
use crate::notes::{self, Links, Note};
use crate::op::Repo;

/// What answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// The signed ledger, on its box.
    Ledger,
    /// The notes ref: what the ledger said when the notes were written,
    /// signed by nothing.
    Notes,
}

/// What Conary says about one line of a checkout.
#[derive(Debug, Clone, Serialize)]
pub struct Why {
    pub repo: Repo,
    /// The commit that last changed the line.
    pub commit: CommitId,
    pub source: Source,
    /// The note of the landing that brought `commit` onto trunk, if there's
    /// one.
    pub landed: Option<Note>,
}

/// The line's repository, the commit that last changed it, and every
/// commit from that one to the checked-out head.
fn blamed(
    dir: &Path,
    file: &Path,
    line: u32,
) -> Result<(Repo, CommitId, HashSet<String>), GitError> {
    let repo = git::github_repo(dir)?;
    let commit = git::blame(dir, file, line)?;
    let after = git::descendants(dir, &commit)?;
    Ok((repo, commit, after))
}

/// Why `line` (from 1) of `file` in the checkout at `dir` is the way it is,
/// as the ledger says. The checkout can be trunk, a branch that merged trunk
/// in, or any commit that holds the landing.
pub fn why(ledger: &Ledger, dir: &Path, file: &Path, line: u32) -> Result<Why, GitError> {
    let (repo, commit, after) = blamed(dir, file, line)?;
    // The ledger is read on its own box, so nothing is left out.
    let notes = notes::landed(ledger, &repo, Links::Kept);
    Ok(Why {
        landed: landing(&repo, notes, &after),
        repo,
        commit,
        source: Source::Ledger,
    })
}

/// Why `line` of `file` is the way it is, as the notes on the checkout's
/// origin say: fetched first, so a fresh clone can answer.
pub fn why_from_notes(dir: &Path, file: &Path, line: u32) -> Result<Why, GitError> {
    let (repo, commit, after) = blamed(dir, file, line)?;
    let mut held = Vec::new();
    if git::fetch_notes(dir, notes::REF)? {
        for (on, text) in git::notes(dir, notes::REF, &after)? {
            let note = Note::parse(&text).map_err(|error| GitError::NotANote {
                commit: on.clone(),
                error,
            })?;
            // A note sits on the trunk commit its landing made.
            if note.landing.after.as_str() != on {
                return Err(GitError::NotANote {
                    commit: on,
                    error: format!("its landing made {}", note.landing.after),
                });
            }
            held.push(note);
        }
    }
    Ok(Why {
        landed: landing(&repo, held, &after),
        repo,
        commit,
        source: Source::Notes,
    })
}

/// The landing that brought a commit onto trunk, given the commit and what
/// came after it (`after`): the one whose trunk after is among them and
/// whose trunk before isn't. A merge into a branch, or a local commit, has
/// no landing of its own and doesn't stand in for one.
fn landing(
    repo: &Repo,
    notes: impl IntoIterator<Item = Note>,
    after: &HashSet<String>,
) -> Option<Note> {
    notes.into_iter().filter(|n| &n.repo == repo).find(|n| {
        after.contains(n.landing.after.as_str()) && !after.contains(n.landing.before.as_str())
    })
}

#[cfg(test)]
mod tests;
