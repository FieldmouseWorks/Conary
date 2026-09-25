// crates/conary-core/src/resolver/sat/end_state.rs

//! Fixed-point validation of hard requirement groups against a transaction's
//! projected end state.
//!
//! A known end state is `(installed - outgoing) + incoming`. SAT may add
//! repository packages, and relation planning may remove more installed troves
//! than the caller declared outgoing, so the set of installed packages the
//! transaction can affect is only complete after a solve. This module loads the
//! bounded candidate universe once as cheap raw facts and admits a candidate
//! only once a selected or removed capability mentions one of its atoms; an
//! unadmitted candidate never incurs architecture resolution or evaluation.

mod candidates;

use resolvo::{Problem, SolvableId, Solver, UnsolvableOrCancelled};
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
use crate::resolver::provider::{ConaryProvider, SolverExpression};

use super::install::{build_expression_requirements, build_provider_for_requirement_expressions};
use super::{
    EndState, SatGroupOwner, SatPackage, SatRelationRemoval, SatResolution, SatSource,
    SatUnsatisfiedGroup,
};
use candidates::{
    InstalledCandidate, affected_capability_names, insert_identity_name,
    installed_hard_group_candidates,
};

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
    let mut fixed = before
        .iter()
        .filter(|package| {
            !package
                .installed_trove_id
                .is_some_and(|trove_id| outgoing.contains(&trove_id))
        })
        .cloned()
        .collect::<Vec<_>>();
    if let Some(incoming) = incoming {
        fixed.push(incoming.clone());
    }
    Ok(EndStateFacts { before, fixed })
}

/// The residual expression one validated group still needs from the solver
/// against the fixed end state (`None` means the fixed state already holds it).
fn end_state_residual(
    group: &ValidatedRequirementGroup,
    fixed_end_state: &[PackageIdentity],
    native_architecture: &str,
    canonical_equivalents: &CanonicalEquivalents,
) -> Result<Option<RepositoryRequirementExpression>> {
    if group.satisfied_against(native_architecture, fixed_end_state, canonical_equivalents)? {
        return Ok(None);
    }
    simplify_against_end_state(
        &group.expression,
        group.version_scheme,
        &group.depending_architecture,
        native_architecture,
        fixed_end_state,
        canonical_equivalents,
    )
}

/// The hard groups validated against the transaction end state and the finite
/// universe of installed candidates they are drawn from.
struct EndStateValidation {
    /// Groups in evaluation order: incoming first, then candidates in persisted
    /// order as they are admitted.
    groups: Vec<ValidatedRequirementGroup>,
    /// Residual against the fixed end state, aligned with `groups`.
    residuals: Vec<Option<RepositoryRequirementExpression>>,
    /// Whether each group in `groups` was promoted to its unsimplified form.
    live: Vec<bool>,
    /// Installed candidates not yet admitted, in persisted order.
    candidates: Vec<InstalledCandidate>,
    /// Whether each candidate has been admitted or permanently dropped.
    admitted: Vec<bool>,
    /// Capability names the transaction adds or removes; only grows.
    affected: HashSet<String>,
    /// Explicit pass bound: every non-final pass promotes a group or admits a
    /// candidate, and each can happen at most once per group.
    max_passes: usize,
    /// Canonical name equivalences shared with SAT candidate filtering.
    canonical_equivalents: CanonicalEquivalents,
}

impl EndStateValidation {
    fn new(
        groups: Vec<ValidatedRequirementGroup>,
        residuals: Vec<Option<RepositoryRequirementExpression>>,
        candidates: Vec<InstalledCandidate>,
        affected: HashSet<String>,
        canonical_equivalents: CanonicalEquivalents,
    ) -> Self {
        let live = vec![false; groups.len()];
        let admitted = vec![false; candidates.len()];
        let max_passes = groups.len() + candidates.len() + 1;
        Self {
            groups,
            residuals,
            live,
            candidates,
            admitted,
            affected,
            max_passes,
            canonical_equivalents,
        }
    }

    /// Admit every not-yet-admitted candidate whose expression mentions an
    /// affected capability, appending it in persisted candidate order.
    ///
    /// Admission is where a candidate's architecture authority is resolved and
    /// its pre-transaction satisfaction is evaluated. A missing authority is a
    /// typed refusal for that admitted group only. A group that was already
    /// unsatisfied before the transaction is dropped permanently so a
    /// pre-existing breakage is never attributed to this transaction. A
    /// candidate whose owner this pass's relation plan removes is left
    /// unadmitted, because the owner is not part of that pass's end state; the
    /// exclusion is per pass, so a later pass that keeps the owner still
    /// considers it. Returns whether any admitted candidate still needs the
    /// solver (non-`None` residual against the fixed state).
    fn admit_mentioned(
        &mut self,
        before: &[PackageIdentity],
        fixed_end_state: &[PackageIdentity],
        native_architecture: &str,
        removed_trove_ids: &HashSet<i64>,
    ) -> Result<bool> {
        let mut newly_mentioned = Vec::new();
        for (index, candidate) in self.candidates.iter().enumerate() {
            if self.admitted[index] {
                continue;
            }
            if removed_trove_ids.contains(&candidate.trove_id) {
                continue;
            }
            if candidate
                .expression
                .atoms()
                .iter()
                .any(|atom| self.affected.contains(&atom.name))
            {
                newly_mentioned.push(index);
            }
        }

        let mut added_unsatisfied = false;
        for index in newly_mentioned {
            // Admission is one-way: a dropped candidate is never reconsidered.
            self.admitted[index] = true;
            let group = self.candidates[index].clone().into_validated()?;
            if !group.satisfied_against(native_architecture, before, &self.canonical_equivalents)? {
                continue;
            }
            let residual = end_state_residual(
                &group,
                fixed_end_state,
                native_architecture,
                &self.canonical_equivalents,
            )?;
            if residual.is_some() {
                added_unsatisfied = true;
            }
            self.groups.push(group);
            self.residuals.push(residual);
            self.live.push(false);
        }
        Ok(added_unsatisfied)
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
        fixed_end_state: &[PackageIdentity],
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
            fixed_end_state,
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
            let residuals = validated
                .iter()
                .map(|group| Some(group.expression.clone()))
                .collect::<Vec<_>>();
            let mut validation = EndStateValidation::new(
                validated,
                residuals,
                Vec::new(),
                HashSet::new(),
                canonical_equivalents,
            );
            solve_validated_groups_to_fixed_point(conn, &mut validation, None, &[], &[], policy, "")
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
            let mut residuals = Vec::with_capacity(validated.len());
            for group in &validated {
                residuals.push(end_state_residual(
                    group,
                    &facts.fixed,
                    &native_architecture,
                    &canonical_equivalents,
                )?);
            }
            let mut validation = EndStateValidation::new(
                validated,
                residuals,
                candidates,
                affected,
                canonical_equivalents,
            );
            validation.admit_mentioned(
                &facts.before,
                &facts.fixed,
                &native_architecture,
                &HashSet::new(),
            )?;
            if validation.groups.is_empty() {
                return Ok(SatResolution::empty());
            }
            policy
                .validate_source_identities()
                .map_err(Error::ConfigError)?;
            if validation.residuals.iter().all(Option::is_none) {
                return Ok(SatResolution::empty());
            }
            if let Some(message) = policy.validate_for_dependency_resolution().err() {
                // Strict mixing with no repository authority admits only the
                // fixed end state itself.
                return Err(Error::ConfigError(message));
            }
            solve_validated_groups_to_fixed_point(
                conn,
                &mut validation,
                Some(facts.fixed.as_slice()),
                &facts.before,
                outgoing_trove_ids,
                policy,
                &native_architecture,
            )
        }
    }
}

/// Iterate SAT passes until every validated hard group holds against the
/// projected end state.
///
/// `groups` starts with the incoming requirements and the affected installed
/// candidates, and grows as each resolved pass mentions new capabilities. Each
/// non-final pass either promotes a group to its unsimplified expression or
/// admits a new candidate, and each can happen at most once per group in the
/// finite candidate universe. The pass counter is bounded by `incoming groups +
/// candidate universe + 1`, so a logic error cannot loop forever.
fn solve_validated_groups_to_fixed_point(
    conn: &Connection,
    validation: &mut EndStateValidation,
    fixed_end_state: Option<&[PackageIdentity]>,
    before: &[PackageIdentity],
    outgoing_trove_ids: &[i64],
    policy: &ResolutionPolicy,
    native_architecture: &str,
) -> Result<SatResolution> {
    // A fixed end state locks the surviving installed candidates; an unknown
    // end state cannot project, so there is nothing to lock or validate.
    let lock_surviving_installed = fixed_end_state.is_some();
    let mut passes = 0;
    loop {
        passes += 1;
        let expressions = compile_group_residuals(&validation.groups, &validation.residuals)?;
        let pass = solve_expression_pass(
            conn,
            &expressions,
            policy,
            outgoing_trove_ids,
            lock_surviving_installed,
        )?;

        let (install_order, remove_order, selected) = match pass {
            ExpressionPass::Conflict(message) => {
                return unsatisfiable_pass_resolution(
                    conn,
                    validation,
                    &message,
                    policy,
                    outgoing_trove_ids,
                    lock_surviving_installed,
                );
            }
            ExpressionPass::Resolved {
                install_order,
                remove_order,
                selected,
            } => (install_order, remove_order, selected),
        };

        // An unknown end state cannot be projected, so the caller's own
        // semantics apply and there is nothing to validate against.
        let Some(fixed_end_state) = fixed_end_state else {
            return Ok(SatResolution::resolved(install_order, remove_order));
        };

        // A pass can mention capabilities only SAT-selected repository packages
        // or relation-removed installed troves provide. Admit any installed
        // candidate those newly mention before validating the projection.
        let added_unsatisfied = validation.extend_from_pass(
            &selected,
            &remove_order,
            before,
            fixed_end_state,
            native_architecture,
        )?;
        let violated = groups_violated_by_solved_end_state(
            fixed_end_state,
            &selected,
            &remove_order,
            &validation.groups,
            native_architecture,
            &validation.canonical_equivalents,
        )?;
        if violated.is_empty() && !added_unsatisfied {
            return Ok(SatResolution::resolved(install_order, remove_order));
        }

        // Promote every newly violated group to its unsimplified expression.
        // Groups already live and the residual vector keep original group
        // order, so every pass is deterministic.
        let mut promoted = false;
        for &index in &violated {
            if !validation.live[index] {
                validation.live[index] = true;
                validation.residuals[index] = Some(validation.groups[index].expression.clone());
                promoted = true;
            }
        }
        if !promoted && !added_unsatisfied {
            return Ok(unsatisfied_groups_conflict(&validation.groups, &violated));
        }
        if passes == validation.max_passes {
            return Ok(unsatisfied_groups_conflict(&validation.groups, &violated));
        }
    }
}

/// Return the indices of original hard groups the projected end state does not
/// satisfy.
///
/// The projected end state is the fixed state plus every package SAT selected,
/// minus the exact installed troves the relation plan removes. A group whose
/// owning installed trove the relation plan removes is itself dropped, because
/// the trove owns no group in that pass's end state. Evaluating the unsimplified
/// groups against it catches a conditional the pre-solve simplification dropped
/// but that SAT then turned true by selecting the condition package. The shared
/// typed evaluator decides satisfaction, so the check uses the same algebra as
/// the fixed-state pass.
fn groups_violated_by_solved_end_state(
    fixed_end_state: &[PackageIdentity],
    selected: &[PackageIdentity],
    remove_order: &[SatRelationRemoval],
    groups: &[ValidatedRequirementGroup],
    native_architecture: &str,
    canonical_equivalents: &CanonicalEquivalents,
) -> Result<Vec<usize>> {
    let removed = removed_trove_ids(remove_order);
    let mut projected = fixed_end_state.to_vec();
    if !removed.is_empty() {
        projected.retain(|package| {
            !package
                .installed_trove_id
                .is_some_and(|trove_id| removed.contains(&trove_id))
        });
    }
    projected.extend(selected.iter().cloned());

    let mut violated = Vec::new();
    for (index, group) in groups.iter().enumerate() {
        if group.owner_removed_by(&removed) {
            continue;
        }
        if !group.satisfied_against(native_architecture, &projected, canonical_equivalents)? {
            violated.push(index);
        }
    }
    Ok(violated)
}

/// One solver pass over compiled root requirement expressions.
enum ExpressionPass {
    Conflict(String),
    Resolved {
        install_order: Vec<SatPackage>,
        remove_order: Vec<SatRelationRemoval>,
        selected: Vec<PackageIdentity>,
    },
}

/// Compile the residual expression of each validated group, skipping discharged
/// groups. The result preserves group order.
fn compile_group_residuals(
    groups: &[ValidatedRequirementGroup],
    residuals: &[Option<RepositoryRequirementExpression>],
) -> Result<Vec<SolverExpression>> {
    groups
        .iter()
        .zip(residuals)
        .filter_map(|(group, residual)| {
            residual.as_ref().map(|expression| {
                crate::resolver::provider::repository_expression_to_solver_for_architecture(
                    expression,
                    group.version_scheme,
                    &group.depending_architecture,
                )
            })
        })
        .collect()
}

/// Run one solve over the given root expressions, returning the relation plan's
/// typed outcome or the selected package facts.
fn solve_expression_pass(
    conn: &Connection,
    expressions: &[SolverExpression],
    policy: &ResolutionPolicy,
    outgoing_trove_ids: &[i64],
    lock_surviving_installed: bool,
) -> Result<ExpressionPass> {
    let mut provider = build_provider_for_requirement_expressions(
        conn,
        expressions,
        policy,
        outgoing_trove_ids,
        lock_surviving_installed,
    )?;
    let requirements = build_expression_requirements(&mut provider, expressions)?;
    let problem = Problem::new().requirements(requirements);
    let mut solver = Solver::new(provider);
    match solver.solve(problem) {
        Ok(solvable_ids) => {
            let relation_plan =
                super::relations::plan_selected_relations(solver.provider(), &solvable_ids)?;
            if let Some(conflict) = relation_plan.conflict {
                return Ok(ExpressionPass::Conflict(conflict));
            }
            Ok(ExpressionPass::Resolved {
                install_order: collect_install_order(solver.provider(), &solvable_ids),
                remove_order: relation_plan.removals,
                selected: collect_selected_identities(solver.provider(), &solvable_ids),
            })
        }
        Err(UnsolvableOrCancelled::Unsolvable(conflict)) => Ok(ExpressionPass::Conflict(
            conflict.display_user_friendly(&solver).to_string(),
        )),
        Err(UnsolvableOrCancelled::Cancelled(_)) => Err(Error::InitError(
            "Dependency resolution was cancelled".to_string(),
        )),
    }
}

/// A refusal naming every hard group the fixed-point iteration could not place
/// in the projected end state, with the typed group identities preserved.
fn unsatisfied_groups_conflict(
    groups: &[ValidatedRequirementGroup],
    violated: &[usize],
) -> SatResolution {
    let unsatisfied = violated
        .iter()
        .map(|&index| groups[index].unsatisfied())
        .collect::<Vec<_>>();
    let descriptions = unsatisfied
        .iter()
        .map(SatUnsatisfiedGroup::description)
        .collect::<Vec<_>>()
        .join("; ");
    SatResolution::conflict_with_groups(
        format!(
            "the solved install order leaves hard requirement group(s) unsatisfied in the transaction end state: {descriptions}"
        ),
        unsatisfied,
    )
}

/// Attribute an unsatisfiable known-end-state pass to the incoming
/// requirements or to the installed packages they break.
///
/// The pass can fail because the incoming requirements alone are unsatisfiable,
/// for example when the incoming package requires a capability no repository
/// provides. Naming every installed group with a residual would misattribute
/// that failure. When installed groups contribute residuals, one diagnostic pass
/// over only the incoming residuals distinguishes the two cases: if it also
/// fails, the incoming requirements are the cause and no installed group is
/// named; if it resolves, the installed groups are the cause.
fn unsatisfiable_pass_resolution(
    conn: &Connection,
    validation: &EndStateValidation,
    solver_message: &str,
    policy: &ResolutionPolicy,
    outgoing_trove_ids: &[i64],
    lock_surviving_installed: bool,
) -> Result<SatResolution> {
    // This function runs for a pass that produced no resolution, and a pass
    // without a resolution has no relation plan and therefore no remove_order.
    // The removal rule is defined against that remove_order, so there is no
    // removed owner to exclude: every installed group below is one the failing
    // pass itself still required in its root expressions. The rule is applied
    // wherever a remove_order exists, in `admit_mentioned` when deciding
    // admissions and in `groups_violated_by_solved_end_state` when validating
    // the pass.
    let installed = validation
        .groups
        .iter()
        .zip(&validation.residuals)
        .filter(|(group, residual)| {
            residual.is_some() && matches!(group.owner, ValidatedGroupOwner::Installed { .. })
        })
        .map(|(group, _)| group.unsatisfied())
        .collect::<Vec<_>>();
    if installed.is_empty() {
        return Ok(SatResolution::conflict(solver_message.to_string()));
    }

    let incoming = validation
        .groups
        .iter()
        .zip(&validation.residuals)
        .filter(|(group, _)| matches!(group.owner, ValidatedGroupOwner::Incoming))
        .filter_map(|(group, residual)| {
            residual.as_ref().map(|expression| {
                crate::resolver::provider::repository_expression_to_solver_for_architecture(
                    expression,
                    group.version_scheme,
                    &group.depending_architecture,
                )
            })
        })
        .collect::<Result<Vec<_>>>()?;
    match solve_expression_pass(
        conn,
        &incoming,
        policy,
        outgoing_trove_ids,
        lock_surviving_installed,
    )? {
        ExpressionPass::Conflict(incoming_message) => Ok(SatResolution::conflict(incoming_message)),
        ExpressionPass::Resolved { .. } => {
            let descriptions = installed
                .iter()
                .map(SatUnsatisfiedGroup::description)
                .collect::<Vec<_>>()
                .join("; ");
            Ok(SatResolution::conflict_with_groups(
                format!(
                    "the transaction cannot satisfy installed package requirement group(s) together with the incoming requirements: {descriptions}; {solver_message}"
                ),
                installed,
            ))
        }
    }
}

/// The exact package identities resolvo selected for the solve.
fn collect_selected_identities(
    provider: &ConaryProvider<'_>,
    solvable_ids: &[SolvableId],
) -> Vec<PackageIdentity> {
    solvable_ids
        .iter()
        .map(|solvable_id| provider.get_solvable(*solvable_id).clone())
        .collect()
}

/// Simplify one requirement expression against the fixed end state, returning
/// the residual the solver must still satisfy (`None` means already true).
///
/// Any sub-expression the end state satisfies is true for the whole
/// transaction, so it is dropped. This makes the solver's model agree with the
/// fixed state without pinning every surviving installed trove as a root
/// requirement: when `bar` survives, `foo if bar` becomes `foo`; when `bar` is
/// absent from both installed state and the incoming package, the implication
/// disappears; and an incoming-provided atom inside a conjunction is removed
/// instead of forcing SAT to find a candidate that does not exist.
///
/// `with`/`without` survive only when the end state does not satisfy them. They
/// compile to one same-provider capability expression evaluated per candidate,
/// so a satisfied one is dropped whole rather than decomposed.
fn simplify_against_end_state(
    expression: &RepositoryRequirementExpression,
    version_scheme: VersionScheme,
    depending_architecture: &str,
    native_architecture: &str,
    end_state: &[PackageIdentity],
    canonical_equivalents: &CanonicalEquivalents,
) -> Result<Option<RepositoryRequirementExpression>> {
    use RepositoryRequirementExpression as Expression;

    match expression {
        // A capability or same-provider expression the fixed end state already
        // satisfies is true for the whole transaction and needs no repository
        // work. Composite nodes decide their own satisfaction structurally, so a
        // sub-expression is evaluated exactly once.
        Expression::Atom(_) | Expression::With { .. } | Expression::Without { .. } => {
            if crate::resolver::requirements::requirement_expression_satisfied_with_canonical_equivalents(
                expression,
                version_scheme,
                depending_architecture,
                native_architecture,
                end_state,
                canonical_equivalents,
            )? {
                Ok(None)
            } else {
                Ok(Some(expression.clone()))
            }
        }
        Expression::And(operands) => {
            let mut rewritten = Vec::with_capacity(operands.len());
            for operand in operands {
                if let Some(operand) = simplify_against_end_state(
                    operand,
                    version_scheme,
                    depending_architecture,
                    native_architecture,
                    end_state,
                    canonical_equivalents,
                )? {
                    rewritten.push(operand);
                }
            }
            Ok(match rewritten.len() {
                0 => None,
                1 => rewritten.pop(),
                _ => Some(Expression::And(rewritten)),
            })
        }
        Expression::Or(operands) => {
            let mut rewritten = Vec::with_capacity(operands.len());
            for operand in operands {
                match simplify_against_end_state(
                    operand,
                    version_scheme,
                    depending_architecture,
                    native_architecture,
                    end_state,
                    canonical_equivalents,
                )? {
                    Some(operand) => rewritten.push(operand),
                    // One true disjunct makes the whole disjunction true.
                    None => return Ok(None),
                }
            }
            Ok(Some(Expression::Or(rewritten)))
        }
        Expression::If {
            requirement,
            condition,
            otherwise,
        } => {
            let branch = if condition_holds_against_end_state(
                condition,
                version_scheme,
                depending_architecture,
                native_architecture,
                end_state,
                canonical_equivalents,
            )? {
                requirement
            } else {
                match otherwise {
                    Some(otherwise) => otherwise,
                    None => return Ok(None),
                }
            };
            simplify_against_end_state(
                branch,
                version_scheme,
                depending_architecture,
                native_architecture,
                end_state,
                canonical_equivalents,
            )
        }
        Expression::Unless {
            requirement,
            condition,
            otherwise,
        } => {
            let branch = if !condition_holds_against_end_state(
                condition,
                version_scheme,
                depending_architecture,
                native_architecture,
                end_state,
                canonical_equivalents,
            )? {
                requirement
            } else {
                match otherwise {
                    Some(otherwise) => otherwise,
                    None => return Ok(None),
                }
            };
            simplify_against_end_state(
                branch,
                version_scheme,
                depending_architecture,
                native_architecture,
                end_state,
                canonical_equivalents,
            )
        }
    }
}

/// Evaluate one condition sub-expression against the fixed end state using the
/// shared typed expression evaluator.
fn condition_holds_against_end_state(
    condition: &RepositoryRequirementExpression,
    version_scheme: VersionScheme,
    depending_architecture: &str,
    native_architecture: &str,
    end_state: &[PackageIdentity],
    canonical_equivalents: &CanonicalEquivalents,
) -> Result<bool> {
    crate::resolver::requirements::requirement_expression_satisfied_with_canonical_equivalents(
        condition,
        version_scheme,
        depending_architecture,
        native_architecture,
        end_state,
        canonical_equivalents,
    )
}

/// Build the install order from the identities resolvo selected.
pub(super) fn collect_install_order(
    provider: &ConaryProvider<'_>,
    solvable_ids: &[SolvableId],
) -> Vec<SatPackage> {
    solvable_ids
        .iter()
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
