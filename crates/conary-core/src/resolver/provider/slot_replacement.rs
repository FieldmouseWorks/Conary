// crates/conary-core/src/resolver/provider/slot_replacement.rs

//! Same-name install-slot replacement of surviving installed facts.
//!
//! A known end state keeps every surviving installed package, but installing a
//! repository package of the same exact name replaces the installed trove whose
//! install slot it occupies: that is how the installer upgrades a dependency
//! (`package_install_slots_match`). The solver models the replacement exactly
//! as the installer makes it:
//!
//! - a repository candidate replaces an installed trove only when that trove is
//!   the one installed package of its name sharing the candidate's slot, it is
//!   not pinned, it has the candidate's version scheme (a dependency install is
//!   an ordinary package change, and the installer refuses a cross-scheme
//!   replacement without an explicit replatform), and the fixed incoming
//!   package does not occupy the slot;
//! - the replacer is exclusive with every other same-name solvable in its slot,
//!   so the solver never keeps the predecessor beside its replacement;
//! - a forced installed root yields to its replacers, as it yields to a
//!   relation remover.
//!
//! An ambiguous slot (several installed packages of the name share it) has no
//! predecessor, so no candidate may replace a trove the installer could not
//! identify exactly.

use std::collections::{BTreeSet, HashMap};

use resolvo::{DenseIndex, SolvableId, VersionSetId};

use crate::error::Result;
use crate::repository::selector::package_install_slots_match;
use crate::resolver::identity::PackageIdentity;

use super::ConaryProvider;
use super::types::ConaryConstraint;

/// Solver state of an end state whose surviving installed packages are fixed
/// facts.
#[derive(Debug, Default)]
pub(crate) struct SurvivingInstalledLock {
    /// Per replacing repository solvable, the constraint that forbids every
    /// other same-name solvable in its install slot.
    replacement_constrains: HashMap<u32, Vec<VersionSetId>>,
}

impl ConaryProvider<'_> {
    fn share_install_slot(&self, installed: &PackageIdentity, incoming: &PackageIdentity) -> bool {
        package_install_slots_match(
            installed.version_scheme,
            installed.architecture.as_deref(),
            incoming.version_scheme,
            incoming.architecture.as_deref(),
            &self.native_architecture,
        )
    }

    /// The installed solvable a repository candidate replaces through its
    /// install slot, when the end state locks surviving installed facts.
    ///
    /// Hidden prior removals count: a trove an earlier pass replaced stays the
    /// predecessor its replacer is re-derived from.
    pub(super) fn slot_predecessor(&self, candidate: SolvableId) -> Option<SolvableId> {
        self.surviving_installed_lock.as_ref()?;
        let package = &self.solvables[candidate.to_index()];
        if package.installed_trove_id.is_some() || Some(candidate) == self.fixed_incoming {
            return None;
        }
        if let Some(incoming) = self.fixed_incoming {
            let incoming = &self.solvables[incoming.to_index()];
            if incoming.name == package.name && self.share_install_slot(incoming, package) {
                return None;
            }
        }
        let mut predecessors = self
            .name_to_solvable_ids
            .get(&package.name)?
            .iter()
            .copied()
            .filter(|&solvable| {
                let installed = &self.solvables[solvable.to_index()];
                installed.installed_trove_id.is_some()
                    && self.share_install_slot(installed, package)
            });
        let predecessor = predecessors.next()?;
        let installed = &self.solvables[predecessor.to_index()];
        if predecessors.next().is_some()
            || installed.installed_pinned
            || installed.version_scheme != package.version_scheme
        {
            return None;
        }
        Some(predecessor)
    }

    /// Every loaded repository candidate that replaces installed `predecessor`.
    pub(super) fn slot_replacers(&self, predecessor: SolvableId) -> BTreeSet<u32> {
        let name = &self.solvables[predecessor.to_index()].name;
        self.name_to_solvable_ids
            .get(name)
            .into_iter()
            .flatten()
            .copied()
            .filter(|&candidate| self.slot_predecessor(candidate) == Some(predecessor))
            .map(SolvableId::into_raw)
            .collect()
    }

    /// Compile each replacer's exclusion of the other same-name solvables in its
    /// install slot. Called once the candidate universe is loaded.
    ///
    /// resolvo forbids every candidate of the name the constraint does not
    /// match, so the allowed set is the exact pool `get_candidates` draws from
    /// minus the replacer's same-slot rivals; providers and canonical
    /// equivalents under other names stay allowed.
    pub(crate) fn compile_slot_replacement_constrains(&mut self) -> Result<()> {
        if self.surviving_installed_lock.is_none() {
            return Ok(());
        }
        let mut constrains = HashMap::new();
        for candidate in self.solvable_ids.clone() {
            if self.slot_predecessor(candidate).is_none() {
                continue;
            }
            let package = self.solvables[candidate.to_index()].clone();
            let name_id = self.intern_name(&package.name)?;
            let allowed = self
                .candidates_for_name(name_id)
                .into_iter()
                .filter(|&other| {
                    let rival = &self.solvables[other.to_index()];
                    other == candidate
                        || rival.name != package.name
                        || !self.share_install_slot(rival, &package)
                })
                .map(SolvableId::into_raw)
                .collect::<BTreeSet<_>>();
            let version_set =
                self.intern_conary_version_set(name_id, ConaryConstraint::ExactSolvables(allowed))?;
            constrains.insert(candidate.into_raw(), vec![version_set]);
        }
        if let Some(lock) = self.surviving_installed_lock.as_mut() {
            lock.replacement_constrains = constrains;
        }
        Ok(())
    }

    /// The slot exclusion a selected solvable imposes, if it is a replacer.
    pub(super) fn slot_replacement_constrains(&self, solvable: SolvableId) -> Vec<VersionSetId> {
        self.surviving_installed_lock
            .as_ref()
            .and_then(|lock| lock.replacement_constrains.get(&solvable.into_raw()))
            .cloned()
            .unwrap_or_default()
    }

    /// The installed troves a selection replaces through install slots.
    pub(crate) fn selected_slot_replacements(&self, selected: &[SolvableId]) -> Vec<i64> {
        selected
            .iter()
            .filter_map(|&candidate| self.slot_predecessor(candidate))
            .filter_map(|predecessor| self.solvables[predecessor.to_index()].installed_trove_id)
            .collect()
    }
}
