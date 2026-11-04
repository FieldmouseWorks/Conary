//! A repo's branches on GitHub, and the commits and merges that move them,
//! all through GitHub's API: what the train builds its candidates with and
//! undo its reverts, so nothing on the box runs git.

use crate::api::ApiError;

/// Conary's own branches are under here: the train's candidates' and
/// undo's reverts, so each can tell its own from anyone else's.
pub const BRANCHES: &str = "conary/";

/// What a merge onto a branch did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Merged {
    /// The merge commit, and its tree.
    Commit { sha: String, tree: String },
    /// The branch already holds that head.
    Already,
    /// It won't merge.
    Conflict,
}

/// A repo's branches, named without `refs/heads/`.
pub trait Branches {
    /// The repo's default branch: trunk, as GitHub says it is now.
    fn default_branch(&self, repo: &str) -> Result<String, ApiError>;
    /// The commit a branch is at, if it's there.
    fn branch(&self, repo: &str, name: &str) -> Result<Option<String>, ApiError>;
    /// Every branch whose name starts with `prefix`.
    fn branches(&self, repo: &str, prefix: &str) -> Result<Vec<String>, ApiError>;
    fn create_branch(&self, repo: &str, name: &str, sha: &str) -> Result<(), ApiError>;
    /// Deletes a branch; one that's already gone is fine.
    fn delete_branch(&self, repo: &str, name: &str) -> Result<(), ApiError>;
    /// Merges `head`, a commit, into the branch `into`.
    fn merge(&self, repo: &str, into: &str, head: &str, message: &str) -> Result<Merged, ApiError>;
    /// A commit's tree.
    fn tree(&self, repo: &str, sha: &str) -> Result<String, ApiError>;
}
