//! Undo in the ledger (`docs/DESIGN.md` section 5): an undo is a change of
//! its own, whose PR reverts landed changes in its repo. It's recorded with
//! what it reverts and what asked for it, and once it lands they're
//! reverted, until it's undone in turn.

use std::collections::BTreeSet;

use serde::Serialize;

use super::{Invalid, Ledger, Status};
use crate::id::ChangeId;
use crate::op::{Asked, Body};

/// What an undo change reverts, and what asked for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Undo {
    pub reverts: Vec<ChangeId>,
    pub asked: Asked,
    pub at: i64,
}

impl Ledger {
    /// An undo's record is refused unless every change it reverts landed in
    /// its repo, has no other undo open or in effect, and is what `asked`
    /// names: each the session's, or each landed after the time.
    pub(super) fn check_undo(
        &self,
        change: ChangeId,
        reverts: &[ChangeId],
        asked: &Asked,
    ) -> Result<(), Invalid> {
        let undo = self.existing(change)?;
        if let Some(was) = &undo.undo {
            return Err(if (&was.reverts[..], &was.asked) == (reverts, asked) {
                Invalid::Duplicate
            } else {
                Invalid::AlreadyUndo
            });
        }
        if reverts.is_empty() {
            return Err(Invalid::NothingToUndo);
        }
        if matches!(asked, Asked::Change) && reverts.len() > 1 {
            return Err(Invalid::NotAsked(reverts[1]));
        }
        let mut named = BTreeSet::from([change]);
        for &id in reverts {
            if !named.insert(id) {
                return Err(Invalid::UndoRepeats(id));
            }
            let c = self.existing(id)?;
            if c.repo != undo.repo {
                return Err(Invalid::OtherRepo(id));
            }
            let Some(landing) = &c.landing else {
                return Err(Invalid::NotLandedYet(id));
            };
            if let Some(by) = self.live_undo(id) {
                return Err(Invalid::AlreadyUndone { change: id, by });
            }
            let asked_for = match asked {
                Asked::Change => true,
                Asked::Actor { session } => c.author.session.as_ref() == Some(session),
                Asked::Since { since } => landing.at > *since,
            };
            if !asked_for {
                return Err(Invalid::NotAsked(id));
            }
        }
        Ok(())
    }

    /// An undo of `id` that's still to be reckoned with: open, or landed
    /// and in effect. A closed one, or one that's been undone itself, is no
    /// bar to another.
    fn live_undo(&self, id: ChangeId) -> Option<ChangeId> {
        self.changes[&id].undone_by.iter().copied().find(|u| {
            let u = &self.changes[u];
            matches!(u.status(), Status::Open | Status::Landed)
        })
    }

    /// Applies an undo op [`Ledger::check`] has accepted: `true` if it was
    /// one.
    pub(super) fn apply_undo(&mut self, body: &Body, at: i64) -> bool {
        let Body::Undo {
            change,
            reverts,
            asked,
        } = body
        else {
            return false;
        };
        for id in reverts {
            let c = self.changes.get_mut(id).expect("check knows it");
            c.undone_by.push(*change);
        }
        let undo = self.changes.get_mut(change).expect("check knows it");
        undo.undo = Some(Undo {
            reverts: reverts.clone(),
            asked: asked.clone(),
            at,
        });
        // Recorded after its PR landed, it's in effect already.
        if undo.landing.is_some() {
            self.landed_undo(*change);
        }
        true
    }

    /// Once `change` lands, what it reverts is reverted, if it's an undo.
    pub(super) fn landed_undo(&mut self, change: ChangeId) {
        if let Some(undo) = &self.changes[&change].undo {
            self.settle(undo.reverts.clone());
        }
    }

    /// Brings each change's reverted state in line with its undos: reverted
    /// by the newest landed undo of it that's in effect, if any. A change
    /// whose state moves is an undo's effect moving too, so what that undo
    /// reverts is settled in turn: an undo of an undo puts its changes back.
    fn settle(&mut self, mut todo: Vec<ChangeId>) {
        while let Some(id) = todo.pop() {
            let c = &self.changes[&id];
            let now = c.undone_by.iter().rev().copied().find(|u| {
                let u = &self.changes[u];
                u.landing.is_some() && u.reverted_by.is_none()
            });
            if now == c.reverted_by {
                continue;
            }
            let c = self.changes.get_mut(&id).expect("it's in the ledger");
            c.reverted_by = now;
            if let Some(undo) = &c.undo {
                todo.extend(undo.reverts.iter().copied());
            }
        }
    }
}

#[cfg(test)]
#[path = "undo/tests.rs"]
mod tests;
