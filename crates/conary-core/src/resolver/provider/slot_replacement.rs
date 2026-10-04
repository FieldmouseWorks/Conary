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
//!   relation remover;
//! - a relation remover is likewise exclusive with every installed trove it
//!   removes, so a pass never both selects an obsoleter and relies on what it
//!   obsoletes.
//!
//! An ambiguous slot (several installed packages of the name share it) has no
//! predecessor, so no candidate may replace a trove the installer could not
//! identify exactly.

use std::collections::{BTreeMap, BTreeSet, HashMap};

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
    /// Per replacing or relation-removing solvable, the constraints that
    /// forbid what it replaces or removes.
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

    /// Compile each replacing candidate's exclusions. Called once the candidate
    /// universe is loaded.
    ///
    /// A slot replacer excludes the other same-name solvables in its install
    /// slot, and a relation remover excludes every installed trove it
    /// relation-removes: neither may coexist with what it replaces in one end
    /// state. resolvo forbids every candidate of a constrained name the
    /// constraint does not match, so each allowed set is the exact pool
    /// `get_candidates` draws from minus the excluded solvables; providers and
    /// canonical equivalents under other names stay allowed.
    pub(crate) fn compile_replacement_constrains(&mut self) -> Result<()> {
        if self.surviving_installed_lock.is_none() {
            return Ok(());
        }
        let mut constrains = HashMap::new();
        for candidate in self.solvable_ids.clone() {
            let package = self.solvables[candidate.to_index()].clone();
            let mut excluded: BTreeMap<String, BTreeSet<SolvableId>> = BTreeMap::new();
            if self.slot_predecessor(candidate).is_some() {
                let rivals = self.name_to_solvable_ids[&package.name]
                    .iter()
                    .copied()
                    .filter(|&other| {
                        other != candidate
                            && self.share_install_slot(&self.solvables[other.to_index()], &package)
                    })
                    .collect::<Vec<_>>();
                excluded
                    .entry(package.name.clone())
                    .or_default()
                    .extend(rivals);
            }
            for removed in self.relation_removed_installed(candidate)? {
                excluded
                    .entry(self.solvables[removed.to_index()].name.clone())
                    .or_default()
                    .insert(removed);
            }
            if excluded.is_empty() {
                continue;
            }
            let mut version_sets = Vec::new();
            for (name, excluded) in excluded {
                let name_id = self.intern_name(&name)?;
                let allowed = self
                    .candidates_for_name(name_id)
                    .into_iter()
                    .filter(|other| !excluded.contains(other))
                    .map(SolvableId::into_raw)
                    .collect::<BTreeSet<_>>();
                version_sets.push(self.intern_conary_version_set(
                    name_id,
                    ConaryConstraint::ExactSolvables(allowed),
                )?);
            }
            constrains.insert(candidate.into_raw(), version_sets);
        }
        if let Some(lock) = self.surviving_installed_lock.as_mut() {
            lock.replacement_constrains = constrains;
        }
        Ok(())
    }

    /// The exclusions a selected solvable imposes, if it replaces or removes
    /// an installed package.
    pub(super) fn replacement_constrains(&self, solvable: SolvableId) -> Vec<VersionSetId> {
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
