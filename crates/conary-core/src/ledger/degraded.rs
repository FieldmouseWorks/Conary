//! Degraded outcomes in the ledger (`docs/DESIGN.md` section 4.6): a path
//! that half-worked for one revision of a change is recorded once while it
//! lasts, and a later op says it works again. Nothing is removed: the
//! change keeps every degradation, cleared or not.

use serde::Serialize;

use super::{Invalid, Ledger};
use crate::id::{ChangeId, RevisionId};
use crate::op::{DEGRADED_WHY, Degraded};

/// One degraded outcome of a change's revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Degradation {
    pub revision: RevisionId,
    pub what: Degraded,
    pub why: String,
    pub at: i64,
    /// When a later op said the path works again, if one has.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restored: Option<i64>,
}

impl super::Change {
    /// The degradations of `revision` no op has cleared yet, oldest first:
    /// what counts as its checks' failure.
    pub fn degraded(&self, revision: RevisionId) -> impl Iterator<Item = &Degradation> {
        self.degradations
            .iter()
            .filter(move |d| d.revision == revision && d.restored.is_none())
    }
}

impl Ledger {
    /// A degradation is refused for a revision the change doesn't have,
    /// with a blank or overlong `why`, or while the same path is already
    /// degraded for that revision.
    pub(super) fn check_degraded(
        &self,
        change: ChangeId,
        revision: RevisionId,
        what: &Degraded,
        why: &str,
    ) -> Result<(), Invalid> {
        let c = self.existing(change)?;
        if c.revision(revision).is_none() {
            return Err(Invalid::UnknownRevision(revision));
        }
        if why.trim().is_empty() || why.len() > DEGRADED_WHY {
            return Err(Invalid::DegradedWhy);
        }
        if c.degraded(revision).any(|d| d.what == *what) {
            return Err(Invalid::Duplicate);
        }
        Ok(())
    }

    /// A restore is refused unless that path is degraded for that revision.
    pub(super) fn check_restored(
        &self,
        change: ChangeId,
        revision: RevisionId,
        what: &Degraded,
    ) -> Result<(), Invalid> {
        if self
            .existing(change)?
            .degraded(revision)
            .any(|d| d.what == *what)
        {
            Ok(())
        } else {
            Err(Invalid::NotDegraded)
        }
    }

    pub(super) fn apply_degraded(
        c: &mut super::Change,
        revision: RevisionId,
        what: &Degraded,
        why: &str,
        at: i64,
    ) {
        c.degradations.push(Degradation {
            revision,
            what: what.clone(),
            why: why.to_owned(),
            at,
            restored: None,
        });
    }

    pub(super) fn apply_restored(
        c: &mut super::Change,
        revision: RevisionId,
        what: &Degraded,
        at: i64,
    ) {
        for d in &mut c.degradations {
            if d.revision == revision && d.what == *what && d.restored.is_none() {
                d.restored = Some(at);
            }
        }
    }
}

#[cfg(test)]
mod tests;
