// crates/conary-core/src/resolver/provider/traits.rs

//! resolvo trait implementations for `ConaryProvider`.
//!
//! Implements the `Interner` and `DependencyProvider` traits that bridge
//! Conary's data model to resolvo's SAT solver interface.
//! Solvables are now `PackageIdentity` instances.

use std::fmt;

use resolvo::{
    Candidates, Condition, ConditionId, DenseIndex, Dependencies, DependencyProvider,
    HintDependenciesAvailable, Interner, KnownDependencies, NameId, SolvableId, SolverCache,
    StringId, VersionSetId, VersionSetUnionId,
};

use super::ConaryProvider;
use super::matching::constraint_matches_candidate;
use super::types::ConaryConstraint;

// --- Display helpers ---

struct DisplayName<'a>(&'a str);
impl fmt::Display for DisplayName<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

struct DisplaySolvable<'a> {
    name: &'a str,
    version: &'a str,
}
impl fmt::Display for DisplaySolvable<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}={}", self.name, self.version)
    }
}

struct DisplayVersionSet<'a>(&'a ConaryConstraint);
impl fmt::Display for DisplayVersionSet<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

struct DisplayString<'a>(&'a str);
impl fmt::Display for DisplayString<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

// --- Interner implementation ---

impl Interner for ConaryProvider<'_> {
    type NameId = NameId;
    type SolvableId = SolvableId;

    fn display_solvable(&self, solvable: SolvableId) -> impl fmt::Display + '_ {
        let pkg = &self.solvables[solvable.to_index()];
        DisplaySolvable {
            name: &pkg.name,
            version: &pkg.version,
        }
    }

    fn display_name(&self, name: NameId) -> impl fmt::Display + '_ {
        DisplayName(&self.names[name.to_index()])
    }

    fn display_version_set(&self, version_set: VersionSetId) -> impl fmt::Display + '_ {
        DisplayVersionSet(&self.version_sets[version_set.to_index()].1)
    }

    fn display_string(&self, string_id: StringId) -> impl fmt::Display + '_ {
        DisplayString(&self.strings[string_id.to_index()])
    }

    fn version_set_name(&self, version_set: VersionSetId) -> NameId {
        self.version_sets[version_set.to_index()].0
    }

    fn solvable_name(&self, solvable: SolvableId) -> NameId {
        let pkg = &self.solvables[solvable.to_index()];
        self.name_to_id[&pkg.name]
    }

    fn version_sets_in_union(
        &self,
        version_set_union: VersionSetUnionId,
    ) -> impl Iterator<Item = VersionSetId> {
        self.version_set_unions
            .get(version_set_union.to_index())
            .expect("resolvo requested a version-set union minted by this provider")
            .clone()
            .into_iter()
    }

    fn resolve_condition(&self, condition: ConditionId) -> Condition {
        self.conditions[condition.as_u32() as usize].clone()
    }
}

// --- DependencyProvider implementation ---

impl ConaryProvider<'_> {
    /// Loaded solvables the solver may choose for `name`, before version-set
    /// filtering.
    ///
    /// This is exactly the candidate pool `get_candidates` hands to
    /// `filter_candidates`. The condition compiler reuses it so a condition's
    /// matching set comes from the same discovery and matching helpers the SAT
    /// requirement path uses.
    pub(super) fn candidates_for_name(&self, name: NameId) -> Vec<SolvableId> {
        let name_str = &self.names[name.to_index()];
        if self.provider_expression_name_ids.contains(&name.into_raw()) {
            return self
                .solvable_ids
                .iter()
                .copied()
                .filter(|&solvable| !self.is_relation_only_installed(solvable))
                .collect();
        }
        let mut candidates = self.solvables_for_name(name);

        // Always include canonical equivalents so the solver can fall back to
        // them when version constraints filter out all exact-name candidates.
        // sort_candidates ranks exact-name matches above canonical ones.
        for equiv in self.canonical_equivalents(name_str) {
            if let Some(&equiv_name_id) = self.name_to_id.get(equiv) {
                let equiv_candidates = self.solvables_for_name(equiv_name_id);
                if !equiv_candidates.is_empty() {
                    tracing::debug!(
                        "Canonical candidates for {}: {} ({} candidates)",
                        name_str,
                        equiv,
                        equiv_candidates.len()
                    );
                    candidates.extend(equiv_candidates);
                }
            }
        }

        // Virtual providers were resolved from typed repository/installed
        // provide rows during provider construction. SAT callbacks only
        // consume those preloaded facts and never fall back to metadata text.
        candidates.extend(self.solvables_for_provide(name_str));

        // A previous pass's relation plan removes these troves from the
        // transaction end state, so they must not satisfy any requirement even
        // though they remain relation-visible for removal re-derivation.
        candidates.retain(|&solvable| !self.is_relation_only_installed(solvable));
        candidates
    }

    /// Apply `version_set` to a candidate pool in either the matching or
    /// inverse direction.
    ///
    /// This is the single matcher `filter_candidates` delegates to and the
    /// condition compiler calls directly, so a compiled condition and a SAT
    /// requirement can never disagree about which solvables satisfy an atom.
    pub(super) fn matching_candidates(
        &self,
        candidates: &[SolvableId],
        version_set: VersionSetId,
        inverse: bool,
    ) -> Vec<SolvableId> {
        let (name_id, ref constraint) = self.version_sets[version_set.to_index()];
        let requested_name = &self.names[name_id.to_index()];
        // The fixed incoming constraint is an exact solvable identity rather
        // than a name/version test, so it bypasses string matching entirely.
        if matches!(constraint, ConaryConstraint::FixedIncoming) {
            return candidates
                .iter()
                .copied()
                .filter(|&sid| (Some(sid) == self.fixed_incoming) != inverse)
                .collect();
        }
        // A compiled condition literal selects exact solvables under their
        // concrete name; membership, not a name/version test, decides it.
        if let ConaryConstraint::ExactSolvables(solvables) = constraint {
            return candidates
                .iter()
                .copied()
                .filter(|&sid| {
                    let matches = solvables.contains(&sid.into_raw())
                        && self.solvables[sid.to_index()].name == *requested_name;
                    if inverse { !matches } else { matches }
                })
                .collect();
        }
        // A fixed installed root selects one exact surviving trove, never a
        // same-name repository candidate.
        if let ConaryConstraint::ExactInstalledTrove(trove_id) = constraint {
            return candidates
                .iter()
                .copied()
                .filter(|&sid| {
                    let package = &self.solvables[sid.to_index()];
                    let matches = package.installed_trove_id == Some(*trove_id)
                        && package.name == *requested_name;
                    if inverse { !matches } else { matches }
                })
                .collect();
        }
        candidates
            .iter()
            .copied()
            .filter(|&sid| {
                let pkg = &self.solvables[sid.to_index()];
                let identity_name_matches = pkg.name == *requested_name
                    || self
                        .canonical_equivalents(requested_name)
                        .iter()
                        .any(|equivalent| equivalent == &pkg.name);
                let matches = constraint_matches_candidate(
                    requested_name,
                    constraint,
                    pkg,
                    &self.native_architecture,
                    identity_name_matches,
                )
                .expect("resolver candidate identity is validated before SAT filtering");
                if inverse { !matches } else { matches }
            })
            .collect()
    }

    /// The fixed incoming candidate when the requested name is its literal
    /// identity. Provided capabilities and canonical identities are not locked:
    /// several packages may provide them.
    fn fixed_incoming_candidate(&self, name: NameId) -> Option<SolvableId> {
        let solvable_id = self.fixed_incoming?;
        let package = &self.solvables[solvable_id.to_index()];
        (package.name == self.names[name.to_index()]).then_some(solvable_id)
    }

    /// The fixed facts of a name: the fixed incoming solvable when its literal
    /// name is `name`, plus every surviving installed variant of `name` when the
    /// end state locks surviving candidates.
    ///
    /// Both are facts of the declared end state, so a requirement on this name
    /// must be satisfiable by selecting them all together, or by a repository
    /// candidate only as an exact install-slot replacer of a surviving variant.
    /// The fixed incoming package always comes first; a
    /// name can be both the incoming's identity and a surviving variant's
    /// identity in a parallel install.
    fn fixed_facts_for_name(&self, name: NameId, candidates: &[SolvableId]) -> Vec<SolvableId> {
        let mut facts = Vec::new();
        if let Some(incoming) = self.fixed_incoming_candidate(name)
            && candidates.contains(&incoming)
        {
            facts.push(incoming);
        }
        if self.surviving_installed_lock.is_some() {
            let name_str = &self.names[name.to_index()];
            for &candidate in candidates {
                let package = &self.solvables[candidate.to_index()];
                if package.name == *name_str
                    && package.installed_trove_id.is_some()
                    && !facts.contains(&candidate)
                {
                    facts.push(candidate);
                }
            }
        }
        facts
    }

    pub(super) fn sort_solvables(&self, solvables: &mut [SolvableId]) {
        // Determine the "primary" name: the first solvable's name is assumed to
        // be the exact-name match. Canonical equivalents have different names and
        // should sort after exact-name candidates.
        let primary_name = solvables
            .first()
            .map(|s| self.solvables[s.to_index()].name.as_str());

        solvables.sort_by(|a, b| {
            let pkg_a = &self.solvables[a.to_index()];
            let pkg_b = &self.solvables[b.to_index()];

            // Exact-name candidates sort before canonical fallbacks
            if let Some(primary) = primary_name {
                let a_exact = pkg_a.name == primary;
                let b_exact = pkg_b.name == primary;
                if a_exact != b_exact {
                    return b_exact.cmp(&a_exact);
                }
            }

            // Higher repository priority is preferred
            if pkg_a.repository_priority != pkg_b.repository_priority {
                return pkg_b.repository_priority.cmp(&pkg_a.repository_priority);
            }

            if pkg_a.version_scheme == pkg_b.version_scheme {
                let version_cmp = crate::repository::versioning::compare_package_identities(
                    pkg_b.version_scheme,
                    &pkg_b.version,
                    pkg_b.package_release.as_deref(),
                    pkg_a.version_scheme,
                    &pkg_a.version,
                    pkg_a.package_release.as_deref(),
                )
                .expect(
                    "resolver package versions and releases are validated before candidate sorting",
                );
                if version_cmp != std::cmp::Ordering::Equal {
                    return version_cmp;
                }
            }

            let a_installed = pkg_a.installed_trove_id.is_some();
            let b_installed = pkg_b.installed_trove_id.is_some();
            b_installed
                .cmp(&a_installed)
                .then_with(|| pkg_a.name.cmp(&pkg_b.name))
                .then_with(|| pkg_a.repository_name.cmp(&pkg_b.repository_name))
        });
    }
}

impl DependencyProvider for ConaryProvider<'_> {
    async fn filter_candidates(
        &self,
        candidates: &[SolvableId],
        version_set: VersionSetId,
        inverse: bool,
    ) -> Vec<SolvableId> {
        self.matching_candidates(candidates, version_set, inverse)
    }

    async fn get_candidates(&self, name: NameId) -> Option<Candidates> {
        let name_str = &self.names[name.to_index()];
        if name_str == "\0conary:false" {
            return None;
        }
        let mut candidates = self.candidates_for_name(name);
        if candidates.is_empty() {
            return None;
        }
        if self.provider_expression_name_ids.contains(&name.into_raw()) {
            return Some(Candidates {
                candidates,
                favored: None,
                locked: None,
                hint_dependencies_available: HintDependenciesAvailable::All,
                excluded: Vec::new(),
                allow_multiple: false,
            });
        }

        let exact_root_candidate = self.root_request_names.contains(name_str)
            && candidates
                .iter()
                .any(|candidate| self.solvables[candidate.to_index()].name == *name_str);
        let favored = self
            .installed_solvable_for_name(name)
            .filter(|solvable_id| candidates.contains(solvable_id))
            .or_else(|| {
                (!exact_root_candidate)
                    .then(|| self.installed_solvable_for_candidates(&candidates))
                    .flatten()
            });

        // The fixed facts of this name are the fixed incoming package and every
        // surviving installed variant of the exact name. They are all part of
        // the declared end state, so a requirement on this name is satisfied by
        // selecting them together, or by a repository candidate only as the
        // installer's exact install-slot upgrade of one surviving variant.
        //
        // If the package is pinned (troves.pinned = 1) and there is no fixed
        // fact, lock the solver to the installed version so the SAT solver
        // cannot choose a different version.  This implements G3: respect
        // per-package version pins.
        let fixed_facts = self.fixed_facts_for_name(name, &candidates);
        let mut allow_multiple = false;
        let mut locked = None;
        if fixed_facts
            .iter()
            .any(|&sid| self.solvables[sid.to_index()].installed_trove_id.is_some())
        {
            // A fixed end state keeps every surviving installed variant of the
            // exact name. A repository candidate of that name never replaces
            // the incoming package, and replaces a surviving variant only as
            // its exact slot replacer, whose compiled constraint excludes the
            // predecessor; every other same-name repository candidate is
            // filtered out. Several facts or a replacer need `allow_multiple`
            // so the solver can select every package the end state holds. A
            // lone surviving variant with no replacer is locked unambiguously.
            candidates.retain(|&sid| {
                let pkg = &self.solvables[sid.to_index()];
                pkg.name != *name_str
                    || pkg.installed_trove_id.is_some()
                    || Some(sid) == self.fixed_incoming
                    || self.slot_predecessor(sid).is_some()
            });
            let has_replacer = candidates
                .iter()
                .any(|&sid| self.slot_predecessor(sid).is_some());
            if fixed_facts.len() > 1 || has_replacer {
                allow_multiple = true;
            } else {
                locked = fixed_facts.first().copied();
            }
        } else if let Some(incoming) = fixed_facts.first() {
            // Only the fixed incoming package is a fact of this name. Its exact
            // root keeps it selected, and the exclusive lock forbids every
            // same-name repository candidate from replacing it.
            locked = Some(*incoming);
        } else {
            locked = candidates.iter().copied().find(|&sid| {
                let pkg = &self.solvables[sid.to_index()];
                pkg.name == *name_str && pkg.installed_pinned
            });
        }

        Some(Candidates {
            candidates,
            favored,
            locked,
            hint_dependencies_available: HintDependenciesAvailable::All,
            excluded: Vec::new(),
            allow_multiple,
        })
    }

    async fn sort_candidates(&self, _solver: &SolverCache<Self>, solvables: &mut [SolvableId]) {
        self.sort_solvables(solvables);
    }

    fn should_cancel_with_value(&self) -> Option<Box<dyn std::any::Any>> {
        self.probe_deadline
            .filter(|deadline| std::time::Instant::now() >= *deadline)
            .map(|_| Box::new(()) as Box<dyn std::any::Any>)
    }

    async fn get_dependencies(&self, solvable: SolvableId) -> Dependencies {
        match self.compiled_dependencies.get(&solvable.into_raw()) {
            Some(requirements) => Dependencies::Known(KnownDependencies {
                requirements: requirements.clone(),
                constrains: self.slot_replacement_constrains(solvable),
            }),
            None => Dependencies::Unknown(self.missing_dependency_authority),
        }
    }
}
