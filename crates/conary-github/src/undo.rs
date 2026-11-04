//! Undo on GitHub (`docs/DESIGN.md` section 5): the PR that reverts a
//! landed change, opened and recorded in the ledger as its undo. The revert
//! is built the way the train builds a candidate, through GitHub's API
//! ([`Branches`]): a commit of the tree trunk had before the change landed,
//! made on its landing, reverts that landing and nothing after it; then
//! trunk is merged into its branch, as GitHub's "update branch" would.
//! Where later work on trunk built on what it reverts, that merge
//! conflicts, and the PR opens without it, conflicting with trunk, for a
//! job to resolve like any PR's conflict.

use conary_core::ledger::{Change, Invalid};
use conary_core::op::{Asked, Body, Repo};
use conary_core::store::StoreError;
use conary_core::{ChangeId, Store};

use crate::api::{ApiError, Source};
use crate::branches::{BRANCHES, Branches, Merged};
use crate::ingest::{self, Config, IngestError};

/// The writes undo needs beyond a repo's [`Branches`].
pub trait Reverts: Branches {
    /// Makes a commit of `tree` on `parent`, as the token's own: its sha.
    fn make_commit(
        &self,
        repo: &str,
        tree: &str,
        parent: &str,
        message: &str,
    ) -> Result<String, ApiError>;
    /// Opens a PR from the branch `head` onto `base`: its number.
    fn open_pull(
        &self,
        repo: &str,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<u64, ApiError>;
    /// The open PR from the repo's own branch `head`, if there is one.
    fn pull_from(&self, repo: &str, head: &str) -> Result<Option<u64>, ApiError>;
}

/// Where the `attempt`th undo of `change` is made. A run that stops
/// before recording its undo leaves the next one the same name, so it
/// takes up what that one made; one recorded, and closed, moves the next
/// on to a fresh branch.
pub fn undo_branch(change: ChangeId, attempt: usize) -> String {
    format!("{BRANCHES}undo/{change}/{attempt}")
}

#[derive(Debug)]
pub enum UndoError {
    Api(ApiError),
    Store(StoreError),
    Ingest(IngestError),
    /// The ledger has no such change.
    Unknown(ChangeId),
    /// It hasn't landed, so there's nothing on trunk to revert.
    NotLanded(ChangeId),
    /// Its undo has landed already: undoing that undo puts it back.
    Reverted {
        change: ChangeId,
        by: ChangeId,
    },
    /// Its undo landed, and `by` undid that, which put it back. A change
    /// takes one undo once one has landed, so undoing `by` reverts it again.
    UndoneUndo {
        change: ChangeId,
        undo: ChangeId,
        by: ChangeId,
    },
    /// The trunk branch isn't there.
    NoTrunk(String),
    /// Trunk already lacks what it brought: its revert would change nothing.
    Nothing(ChangeId),
}

impl std::fmt::Display for UndoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Api(e) => e.fmt(f),
            Self::Store(e) => e.fmt(f),
            Self::Ingest(e) => e.fmt(f),
            Self::Unknown(id) => write!(f, "change {id} isn't in the ledger"),
            Self::NotLanded(id) => write!(f, "change {id} hasn't landed"),
            Self::Reverted { change, by } => write!(
                f,
                "change {change} is reverted already, by {by}: undo that to put it back"
            ),
            Self::UndoneUndo { change, undo, by } => write!(
                f,
                "change {change}'s undo {undo} landed and {by} undid it: undo {by} to revert it again"
            ),
            Self::NoTrunk(name) => write!(f, "no trunk branch {name}"),
            Self::Nothing(id) => write!(
                f,
                "trunk already lacks what change {id} brought, so reverting it changes nothing"
            ),
        }
    }
}

impl std::error::Error for UndoError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Api(e) => Some(e),
            Self::Ingest(e) => Some(e),
            _ => None,
        }
    }
}

impl From<ApiError> for UndoError {
    fn from(e: ApiError) -> Self {
        Self::Api(e)
    }
}

impl From<StoreError> for UndoError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

impl From<IngestError> for UndoError {
    fn from(e: IngestError) -> Self {
        Self::Ingest(e)
    }
}

/// How the undo's PR came to be open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    /// Opened now, and whether it conflicts with trunk as it is.
    Opened { conflicts: bool },
    /// Open already, from an earlier run.
    Found,
}

/// The undo, open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Undone {
    /// The undo's own change, and its PR.
    pub change: ChangeId,
    pub pr: u64,
    pub reverts: Vec<ChangeId>,
    pub how: How,
    /// Ops written; 0 when the ledger already held everything.
    pub recorded: usize,
}

/// Opens the PR that reverts `change`'s landing, and records it as its
/// undo. Run again, it finds the undo it opened, in the ledger or on
/// GitHub, rather than open another.
pub fn undo(
    store: &mut Store,
    forge: &dyn Reverts,
    source: &dyn Source,
    config: &Config,
    change: ChangeId,
) -> Result<Undone, UndoError> {
    let ledger = store.ledger();
    let x = ledger.change(change).ok_or(UndoError::Unknown(change))?;
    let landing = x.landing.clone().ok_or(UndoError::NotLanded(change))?;
    if let Some(by) = x.reverted_by {
        return Err(UndoError::Reverted { change, by });
    }
    // The ledger takes one undo at a time, and once one has landed it's the
    // only one. Since this change isn't reverted, its undo is open still, or
    // landed and undone in turn.
    if let Some(u) = ledger.undo_of(change).and_then(|u| ledger.change(u)) {
        if let Some(by) = u.reverted_by {
            return Err(UndoError::UndoneUndo {
                change,
                undo: u.id,
                by,
            });
        }
        return Ok(Undone {
            change: u.id,
            pr: u.pr,
            reverts: vec![change],
            how: How::Found,
            recorded: 0,
        });
    }
    let repo = x.repo.clone();
    let full = repo.to_string();
    let branch = undo_branch(change, x.undone_by.len() + 1);
    let title = title(x);
    let pr_of_x = x.pr;
    if let Some(pr) = forge.pull_from(&full, &branch)? {
        return record(store, source, config, &repo, pr, change, How::Found);
    }

    let trunk_name = forge.default_branch(&full)?;
    let trunk = forge
        .branch(&full, &trunk_name)?
        .ok_or_else(|| UndoError::NoTrunk(trunk_name.clone()))?;
    // A branch already there is an earlier run's, which stopped before its
    // PR: the same revert, on what trunk was then.
    if forge.branch(&full, &branch)?.is_none() {
        let before = forge.tree(&full, landing.before.as_str())?;
        let message = format!(
            "{title}\n\nThis reverts {}, #{pr_of_x}'s landing.",
            landing.after
        );
        let revert = forge.make_commit(&full, &before, landing.after.as_str(), &message)?;
        forge.create_branch(&full, &branch, &revert)?;
    }
    let message = format!("Merge {trunk_name} into {branch}");
    let tree = match forge.merge(&full, &branch, &trunk, &message)? {
        Merged::Commit { tree, .. } => Some(tree),
        Merged::Already => {
            let head = forge
                .branch(&full, &branch)?
                .ok_or_else(|| ApiError::new(format!("{full}'s {branch} went as it was made")))?;
            Some(forge.tree(&full, &head)?)
        }
        Merged::Conflict => None,
    };
    let conflicts = tree.is_none();
    if let Some(tree) = tree
        && tree == forge.tree(&full, &trunk)?
    {
        forge.delete_branch(&full, &branch)?;
        return Err(UndoError::Nothing(change));
    }
    let body = body(
        pr_of_x,
        change,
        &landing.after.to_string(),
        &trunk_name,
        conflicts,
    );
    let pr = forge.open_pull(&full, &branch, &trunk_name, &title, &body)?;
    record(
        store,
        source,
        config,
        &repo,
        pr,
        change,
        How::Opened { conflicts },
    )
}

/// Ingests the undo's PR and records it as `reverts`' undo.
fn record(
    store: &mut Store,
    source: &dyn Source,
    config: &Config,
    repo: &Repo,
    pr: u64,
    reverts: ChangeId,
    how: How,
) -> Result<Undone, UndoError> {
    let got = ingest::pull(store, source, config, repo, pr)?;
    let mut recorded = got.recorded;
    match store.record(Body::Undo {
        change: got.change,
        reverts: vec![reverts],
        asked: Asked::Change,
    }) {
        Ok(_) => recorded += 1,
        Err(StoreError::Invalid(Invalid::Duplicate)) => {}
        Err(e) => return Err(e.into()),
    }
    Ok(Undone {
        change: got.change,
        pr,
        reverts: vec![reverts],
        how,
        recorded,
    })
}

/// The undo's title, in the style of what it reverts: `area: sentence`
/// becomes `area: undo #N, sentence`, the house style its repo's subjects
/// keep.
fn title(x: &Change) -> String {
    let summary = x.intent.as_ref().map(|i| i.summary.as_str());
    match summary.map(|s| s.split_once(": ")) {
        Some(Some((area, sentence))) => format!("{area}: undo #{}, {sentence}", x.pr),
        Some(None) => format!("undo: #{}, {}", x.pr, summary.unwrap_or_default()),
        None => format!("undo: #{}", x.pr),
    }
}

fn body(pr: u64, change: ChangeId, after: &str, trunk: &str, conflicts: bool) -> String {
    let mut body = format!(
        "Undoes #{pr}, which landed as {after} (Conary change `{change}`), by reverting that landing."
    );
    if conflicts {
        body += &format!(
            "\n\nWhat came after #{pr} on {trunk} builds on it, so this conflicts with {trunk}: \
             merge {trunk} in and resolve each conflict, keeping what came after #{pr} and \
             taking out what #{pr} brought."
        );
    }
    body
}

#[cfg(test)]
mod tests;
