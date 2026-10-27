//! `why`: the change that landed a line (`docs/DESIGN.md` section 7).

use std::path::Path;

use crate::git::{self, GitError};
use crate::id::CommitId;
use crate::ledger::{Change, Ledger};
use crate::op::Repo;

/// What the ledger says about one line of a checkout.
pub struct Why<'a> {
    pub repo: Repo,
    /// The commit that last changed the line.
    pub commit: CommitId,
    /// The trunk commit that landed it: the commit itself, or the merge that
    /// brought it in.
    pub trunk: CommitId,
    /// The change whose landing made `trunk`, if the ledger holds one.
    pub change: Option<&'a Change>,
}

/// Why `line` (from 1) of `file` in the checkout at `dir` is the way it is.
pub fn why<'a>(
    ledger: &'a Ledger,
    dir: &Path,
    file: &Path,
    line: u32,
) -> Result<Why<'a>, GitError> {
    let repo = git::github_repo(dir)?;
    let commit = git::blame(dir, file, line)?;
    if let Some(change) = ledger.landed_as(&repo, &commit) {
        return Ok(Why {
            repo,
            trunk: commit.clone(),
            commit,
            change: Some(change),
        });
    }
    let trunk = git::merged_by(dir, &commit)?.unwrap_or_else(|| commit.clone());
    Ok(Why {
        change: ledger.landed_as(&repo, &trunk),
        repo,
        commit,
        trunk,
    })
}
