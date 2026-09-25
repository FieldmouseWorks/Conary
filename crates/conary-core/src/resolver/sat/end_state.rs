// crates/conary-core/src/resolver/sat/end_state.rs

//! Fixed-point validation of hard requirement groups against a transaction's
//! projected end state.
//!
//! A known end state is `(installed - outgoing) + incoming`. SAT may add
//! repository packages, and relation planning may remove more installed troves
//! than the caller declared outgoing, so the set of installed packages the
//! transaction can affect is only complete after a solve. Every surviving
//! installed package the transaction's groups can observe is rooted as the
//! disjunction of its exact identity and the loaded candidates that
//! relation-remove it, so its stored dependencies are enforced natively by SAT
//! unless a selected obsoleter removes it; the affected capability set decides
//! which installed packages are observed.
//!
//! The fixed-point pass driver itself lives in `fixed_point`, which decides
//! which validated groups each pass compiles into SAT roots.

mod candidates;
mod fixed_point;

use resolvo::SolvableId;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};

use crate::error::{Error, Result};
use crate::repository::dependency_model::{
    RepositoryRequirementExpression, RepositoryRequirementGroup, RepositoryRequirementKind,
};
use crate::repository::resolution_policy::ResolutionPolicy;
use crate::repository::versioning::VersionScheme;
use crate::resolver::canonical::{CanonicalEquivalents, load_canonical_equivalents};
use crate::resolver::identity::PackageIdentity;
use crate::resolver::provider::ConaryProvider;
use crate::resolver::provider::types::RequirementGroupIdentity;

use super::{
    EndState, SatGroupOwner, SatPackage, SatRelationRemoval, SatResolution, SatSource,
    SatUnsatisfiedGroup,
};
use candidates::{
    InstalledCandidate, affected_capability_names, insert_identity_name,
    installed_hard_group_candidates,
};
use fixed_point::{PassContext, solve_validated_groups_to_fixed_point};

/// Whose stored hard requirement group is validated against the end state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ValidatedGroupOwner {
    /// The incoming package's own external requirement.
    Incoming,
    /// A surviving installed package, named for conflict attribution.
    Installed { trove_id: i64, package_name: String },
}

/// One hard requirement group the fixed-point loop projects against the
/// transaction end state.
#[derive(Clone)]
pub(super) struct ValidatedRequirementGroup {
    pub(super) expression: RepositoryRequirementExpression,
    pub(super) native_text: Option<String>,
    pub(super) version_scheme: VersionScheme,
    pub(super) depending_architecture: String,
    pub(super) owner: ValidatedGroupOwner,
}

impl ValidatedRequirementGroup {
    fn satisfied_against(
        &self,
        native_architecture: &str,
        packages: &[PackageIdentity],
        canonical_equivalents: &CanonicalEquivalents,
    ) -> Result<bool> {
        crate::resolver::requirements::requirement_expression_satisfied_with_canonical_equivalents(
            &self.expression,
            self.version_scheme,
            &self.depending_architecture,
            native_architecture,
            packages,
            canonical_equivalents,
        )
    }

    /// Whether the pass's relation plan removes this group's owning installed
    /// trove, so the group is not part of that pass's end state.
    fn owner_removed_by(&self, removed_trove_ids: &HashSet<i64>) -> bool {
        match &self.owner {
            ValidatedGroupOwner::Installed { trove_id, .. } => removed_trove_ids.contains(trove_id),
            ValidatedGroupOwner::Incoming => false,
        }
    }

    /// The typed identity of this group in a refusal, preserving its owner.
    fn unsatisfied(&self) -> SatUnsatisfiedGroup {
        SatUnsatisfiedGroup {
            owner: match &self.owner {
                ValidatedGroupOwner::Incoming => SatGroupOwner::Incoming,
                ValidatedGroupOwner::Installed {
                    trove_id,
                    package_name,
                } => SatGroupOwner::Installed {
                    trove_id: *trove_id,
                    package_name: package_name.clone(),
                },
            },
            native_text: self.native_text.clone(),
            expression: self.expression.clone(),
        }
    }
}

/// The pre-transaction installed facts and the projected fixed end state.
struct EndStateFacts {
    /// Every installed package trove before the transaction removes anything.
    before: Vec<PackageIdentity>,
    /// `before - outgoing`, the installed packages the transaction keeps.
    surviving: Vec<PackageIdentity>,
    /// `(before - outgoing) + incoming`, package troves only.
    fixed: Vec<PackageIdentity>,
}

/// Load the pre-transaction installed package facts and project the fixed end
/// state `(installed - outgoing) + incoming`.
///
/// Only package-type troves are facts. Collections created by
/// `conary collection create` have no architecture, so including them makes the
/// typed evaluator reject the whole set instead of evaluating the group.
fn end_state_facts(
    conn: &Connection,
    outgoing_trove_ids: &[i64],
    incoming: Option<&PackageIdentity>,
) -> Result<EndStateFacts> {
    let before =
        crate::resolver::requirements::load_installed_package_identities_for_packages(conn)?;
    let outgoing = outgoing_trove_ids.iter().copied().collect::<HashSet<_>>();
    let surviving = before
        .iter()
        .filter(|package| {
            !package
                .installed_trove_id
                .is_some_and(|trove_id| outgoing.contains(&trove_id))
        })
        .cloned()
        .collect::<Vec<_>>();
    let mut fixed = surviving.clone();
    if let Some(incoming) = incoming {
        fixed.push(incoming.clone());
    }
    Ok(EndStateFacts {
        before,
        surviving,
        fixed,
    })
}

/// One surviving installed package the transaction's groups can observe, forced
/// as a SAT root that keeps it unless a loaded candidate relation-removes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ForcedInstalledTrove {
    pub(super) trove_id: i64,
}

/// The hard groups validated against the transaction end state and the finite
/// universe of installed candidates they are drawn from.
struct EndStateValidation {
    /// Groups in evaluation order: incoming first, then admitted installed
    /// groups. Every group carries a full expression; the pass driver decides
    /// which owners are rooted on each pass.
    groups: Vec<ValidatedRequirementGroup>,
    /// Installed candidates not yet admitted, in persisted order.
    candidates: Vec<InstalledCandidate>,
    /// Whether each candidate has been admitted or permanently discharged.
    admitted: Vec<bool>,
    /// Surviving installed packages the transaction's groups can observe.
    forced_installed: Vec<ForcedInstalledTrove>,
    /// Installed groups already unsatisfied before the transaction, discharged
    /// by identity so their forced owner never becomes unsatisfiable.
    ignored_groups: HashSet<RequirementGroupIdentity>,
    /// Capability names the transaction adds or removes; only grows.
    affected: HashSet<String>,
    /// Explicit pass bound. Every non-final pass admits a candidate, grows the
    /// hidden removal set, or newly roots a validated group; restoring a stale
    /// hidden trove shrinks it. All moves are finite, so the loop takes at most
    /// `groups + 2 * candidates + 1` passes before the defensive bound refuses.
    max_passes: usize,
    /// Canonical name equivalences shared with SAT candidate filtering.
    canonical_equivalents: CanonicalEquivalents,
}

impl EndStateValidation {
    fn new(
        groups: Vec<ValidatedRequirementGroup>,
        candidates: Vec<InstalledCandidate>,
        affected: HashSet<String>,
        canonical_equivalents: CanonicalEquivalents,
    ) -> Self {
        let admitted = vec![false; candidates.len()];
        let max_passes = groups.len() + 2 * candidates.len() + 1;
        Self {
            groups,
            candidates,
            admitted,
            forced_installed: Vec::new(),
            ignored_groups: HashSet::new(),
            affected,
            max_passes,
            canonical_equivalents,
        }
    }

    /// Recompute the surviving installed packages the transaction's groups can
    /// observe: a surviving package whose literal name, one of its provided
    /// capabilities, or a canonical equivalent of its name appears in the
    /// affected capability set.
    fn refresh_forced_installed(&mut self, surviving: &[PackageIdentity]) {
        let mut forced = Vec::new();
        for package in surviving {
            let Some(trove_id) = package.installed_trove_id else {
                continue;
            };
            let observed = self.affected.contains(&package.name)
                || self
                    .canonical_equivalents
                    .for_name(&package.name)
                    .iter()
                    .any(|equivalent| self.affected.contains(equivalent))
                || package
                    .provided_capabilities
                    .iter()
                    .any(|capability| self.affected.contains(&capability.name));
            if observed {
                forced.push(ForcedInstalledTrove { trove_id });
            }
        }
        self.forced_installed = forced;
    }

    /// Admit every not-yet-admitted candidate the transaction can observe,
    /// appending it in persisted candidate order.
    ///
    /// A candidate is observed when its owner is a forced installed package or
    /// its group expression mentions an affected capability. Admission is where
    /// a candidate's architecture authority is resolved and its pre-transaction
    /// satisfaction is evaluated. A missing authority is a typed refusal for
    /// that admitted group only. A group that was already unsatisfied before
    /// the transaction is discharged permanently by identity so a pre-existing
    /// breakage is never attributed to this transaction and never makes a
    /// forced owner unsatisfiable. A candidate whose owner this pass's relation
    /// plan removes is left unadmitted. Returns whether any admitted candidate
    /// contributed a group that still needs the solver.
    fn admit_mentioned(
        &mut self,
        before: &[PackageIdentity],
        surviving: &[PackageIdentity],
        native_architecture: &str,
        removed_trove_ids: &HashSet<i64>,
    ) -> Result<bool> {
        self.refresh_forced_installed(surviving);
        let mut newly_mentioned = Vec::new();
        for (index, candidate) in self.candidates.iter().enumerate() {
            if self.admitted[index] {
                continue;
            }
            if removed_trove_ids.contains(&candidate.trove_id) {
                continue;
            }
            let owner_forced = self
                .forced_installed
                .iter()
                .any(|forced| forced.trove_id == candidate.trove_id);
            if owner_forced
                || candidate
                    .expression
                    .atoms()
                    .iter()
                    .any(|atom| self.affected.contains(&atom.name))
            {
                newly_mentioned.push(index);
            }
        }

        let mut admitted_group = false;
        for index in newly_mentioned {
            // Admission is one-way: a discharged candidate is never
            // reconsidered.
            self.admitted[index] = true;
            let candidate = self.candidates[index].clone();
            let group = candidate.clone().into_validated()?;
            if !group.satisfied_against(native_architecture, before, &self.canonical_equivalents)? {
                self.ignored_groups
                    .insert(RequirementGroupIdentity::Installed {
                        trove_id: candidate.trove_id,
                        requirement_group_id: candidate.requirement_group_id,
                    });
                continue;
            }
            // A rooted group's atoms, including condition atoms, are observed
            // by the transaction, so they can force the installed packages they
            // name on a later pass.
            for atom in candidate.expression.atoms() {
                self.affected.insert(atom.name.clone());
            }
            self.groups.push(group);
            admitted_group = true;
        }
        if admitted_group {
            self.refresh_forced_installed(surviving);
        }
        Ok(admitted_group)
    }

    /// Extend the affected capability set with every selected identity and every
    /// removed installed trove, then admit newly mentioned candidates.
    ///
    /// A selected or removed package contributes its canonical equivalents
    /// alongside its literal identity name, so an installed dependent that names
    /// a sibling implementation is admitted. Provided-capability names stay
    /// literal.
    fn extend_from_pass(
        &mut self,
        selected: &[PackageIdentity],
        remove_order: &[SatRelationRemoval],
        before: &[PackageIdentity],
        surviving: &[PackageIdentity],
        native_architecture: &str,
    ) -> Result<bool> {
        for package in selected {
            insert_identity_name(
                &mut self.affected,
                &package.name,
                &self.canonical_equivalents,
            );
            for capability in &package.provided_capabilities {
                self.affected.insert(capability.name.clone());
            }
        }
        if !remove_order.is_empty() {
            let installed = before
                .iter()
                .filter_map(|package| {
                    package
                        .installed_trove_id
                        .map(|trove_id| (trove_id, package))
                })
                .collect::<HashMap<_, _>>();
            for removal in remove_order {
                insert_identity_name(
                    &mut self.affected,
                    &removal.package.name,
                    &self.canonical_equivalents,
                );
                if let Some(package) = installed.get(&removal.trove_id) {
                    for capability in &package.provided_capabilities {
                        self.affected.insert(capability.name.clone());
                    }
                }
            }
        }
        self.admit_mentioned(
            before,
            surviving,
            native_architecture,
            &removed_trove_ids(remove_order),
        )
    }
}

/// The installed trove identities a pass's relation plan removes.
fn removed_trove_ids(remove_order: &[SatRelationRemoval]) -> HashSet<i64> {
    remove_order
        .iter()
        .map(|removal| removal.trove_id)
        .collect()
}

/// Solve exact hard requirement groups against the transaction end state.
///
/// `groups` are the incoming package's external hard groups. For a known end
/// state the surviving installed packages' stored hard groups are validated
/// against the same projected end state, so a fresh install cannot silently
/// break an installed package. An unknown end state keeps the incoming-only
/// semantics.
pub(super) fn solve_requirement_groups_for_end_state(
    conn: &Connection,
    groups: &[RepositoryRequirementGroup],
    version_scheme: VersionScheme,
    depending_architecture: &str,
    end_state: EndState<'_>,
    incoming: Option<&PackageIdentity>,
    policy: &ResolutionPolicy,
) -> Result<SatResolution> {
    let mut validated = Vec::new();
    for group in groups {
        crate::repository::requirement::validate_requirement_group(group, version_scheme)
            .map_err(Error::ConfigError)?;
        match group.kind {
            RepositoryRequirementKind::Depends | RepositoryRequirementKind::PreDepends => {
                validated.push(ValidatedRequirementGroup {
                    expression: group.expression.clone(),
                    native_text: group.native_text.clone(),
                    version_scheme,
                    depending_architecture: depending_architecture.to_string(),
                    owner: ValidatedGroupOwner::Incoming,
                });
            }
            RepositoryRequirementKind::Optional
            | RepositoryRequirementKind::Recommends
            | RepositoryRequirementKind::Suggests
            | RepositoryRequirementKind::Supplements
            | RepositoryRequirementKind::Enhances
            | RepositoryRequirementKind::Build => {}
            RepositoryRequirementKind::Conflict
            | RepositoryRequirementKind::Breaks
            | RepositoryRequirementKind::Replace
            | RepositoryRequirementKind::Obsolete => {
                return Err(Error::ConfigError(format!(
                    "negative relation '{}' cannot be solved as a positive install requirement",
                    group.kind.as_str()
                )));
            }
        }
    }

    let canonical_equivalents = load_canonical_equivalents(conn)?;

    match end_state {
        EndState::Unknown => {
            if validated.is_empty() {
                return Ok(SatResolution::empty());
            }
            // A malformed source identity is always a hard error; installed
            // satisfaction never repairs an invalid policy.
            policy
                .validate_source_identities()
                .map_err(Error::ConfigError)?;
            if let Some(message) = policy.validate_for_dependency_resolution().err() {
                return Err(Error::ConfigError(message));
            }
            let mut validation = EndStateValidation::new(
                validated,
                Vec::new(),
                HashSet::new(),
                canonical_equivalents,
            );
            // An unknown end state adds no fixed incoming solvable: the caller
            // has not declared its outgoing set, so the incoming package stays
            // a request-only fact and the current single-pass semantics hold.
            let context = PassContext {
                conn,
                policy,
                incoming: None,
                outgoing_trove_ids: &[],
                lock_surviving_installed: false,
            };
            solve_validated_groups_to_fixed_point(&context, &mut validation, None, &[], &[], "")
        }
        EndState::Known { outgoing_trove_ids } => {
            let native_architecture = crate::repository::registry::detect_system_arch()?;
            let facts = end_state_facts(conn, outgoing_trove_ids, incoming)?;
            let affected = affected_capability_names(
                outgoing_trove_ids,
                incoming,
                &validated,
                &facts.before,
                &canonical_equivalents,
            );
            if affected.is_empty() {
                return Ok(SatResolution::empty());
            }
            let candidates =
                installed_hard_group_candidates(conn, outgoing_trove_ids, &facts.before)?;
            let mut validation =
                EndStateValidation::new(validated, candidates, affected, canonical_equivalents);
            validation.admit_mentioned(
                &facts.before,
                &facts.surviving,
                &native_architecture,
                &HashSet::new(),
            )?;
            if validation.groups.is_empty() {
                return Ok(SatResolution::empty());
            }
            policy
                .validate_source_identities()
                .map_err(Error::ConfigError)?;
            // The fixed end state already holds every group, so no repository
            // work is needed and strict mixing is satisfied.
            let mut fixed_holds_all = true;
            for group in &validation.groups {
                if !group.satisfied_against(
                    &native_architecture,
                    &facts.fixed,
                    &validation.canonical_equivalents,
                )? {
                    fixed_holds_all = false;
                    break;
                }
            }
            if fixed_holds_all {
                return Ok(SatResolution::empty());
            }
            if let Some(message) = policy.validate_for_dependency_resolution().err() {
                // Strict mixing with no repository authority admits only the
                // fixed end state itself.
                return Err(Error::ConfigError(message));
            }
            let context = PassContext {
                conn,
                policy,
                incoming,
                outgoing_trove_ids,
                lock_surviving_installed: true,
            };
            solve_validated_groups_to_fixed_point(
                &context,
                &mut validation,
                Some(facts.fixed.as_slice()),
                &facts.before,
                &facts.surviving,
                &native_architecture,
            )
        }
    }
}

/// Build the install order from the identities resolvo selected.
///
/// The fixed incoming solvable is a transaction fact, not an installed or
/// repository package, so it never appears in the install order.
pub(super) fn collect_install_order(
    provider: &ConaryProvider<'_>,
    solvable_ids: &[SolvableId],
) -> Vec<SatPackage> {
    let fixed_incoming = provider.fixed_incoming_solvable();
    solvable_ids
        .iter()
        .filter(|sid| Some(**sid) != fixed_incoming)
        .map(|sid| {
            let pkg = provider.get_solvable(*sid);
            SatPackage {
                name: pkg.name.clone(),
                version: pkg.version.clone(),
                package_release: pkg.package_release.clone(),
                architecture: pkg.architecture.clone(),
                version_scheme: pkg.version_scheme,
                repo_package_id: pkg.repo_package_id,
                repository_id: pkg.repository_id,
                repository_name: (!pkg.repository_name.is_empty())
                    .then(|| pkg.repository_name.clone()),
                installed_trove_id: pkg.installed_trove_id,
                source: if pkg.installed_trove_id.is_some() {
                    SatSource::Installed
                } else {
                    SatSource::Repository
                },
            }
        })
        .collect()
}

/// Build the install order for a known end state: only the packages the
/// transaction newly installs.
///
/// Every surviving installed package the transaction can observe is a fixed
/// SAT root, so resolvo selects it as a fact of the projected end state. Such a
/// package is already installed and must never be reported as something to
/// install.
pub(super) fn collect_new_install_order(
    provider: &ConaryProvider<'_>,
    solvable_ids: &[SolvableId],
) -> Vec<SatPackage> {
    collect_install_order(provider, solvable_ids)
        .into_iter()
        .filter(|package| package.installed_trove_id.is_none())
        .collect()
}
