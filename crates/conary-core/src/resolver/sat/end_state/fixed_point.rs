// crates/conary-core/src/resolver/sat/end_state/fixed_point.rs

//! The SAT fixed-point driver that validates hard requirement groups against a
//! transaction's projected end state.
//!
//! The first pass is incoming-only. Relation removals are only known after a
//! pass resolves, so an installed group whose owner a later relation plan
//! removes must not make the preliminary pass unsolvable. Each later pass makes
//! the installed groups whose owners the previous pass did not remove into SAT
//! roots. Admission and validation always use the current pass's own removal
//! set, so an owner removed by the pass that solved it never contributes a
//! violation.

use std::collections::HashSet;

use resolvo::{Problem, SolvableId, Solver, UnsolvableOrCancelled};
use rusqlite::Connection;

use crate::error::{Error, Result};
use crate::repository::dependency_model::RepositoryRequirementExpression;
use crate::repository::resolution_policy::ResolutionPolicy;
use crate::resolver::canonical::CanonicalEquivalents;
use crate::resolver::identity::PackageIdentity;
use crate::resolver::provider::{ConaryProvider, SolverExpression};

use super::super::install::{
    build_expression_requirements, build_provider_for_requirement_expressions,
};
use super::super::relations::plan_selected_relations;
use super::super::{SatPackage, SatRelationRemoval, SatResolution, SatUnsatisfiedGroup};
use super::{
    EndStateValidation, ValidatedGroupOwner, ValidatedRequirementGroup, collect_install_order,
    removed_trove_ids,
};

/// The SAT root selection for one pass.
///
/// The first pass keeps installed-owned groups out of the root set entirely;
/// later passes include them except the owners the previous pass removed.
struct PassRoots<'a> {
    include_installed: bool,
    excluded_trove_ids: &'a HashSet<i64>,
}

impl PassRoots<'_> {
    /// Whether `group` is a SAT root under this selection.
    fn includes(&self, group: &ValidatedRequirementGroup) -> bool {
        (self.include_installed || matches!(group.owner, ValidatedGroupOwner::Incoming))
            && !group.owner_removed_by(self.excluded_trove_ids)
    }
}

/// Iterate SAT passes until every validated hard group holds against the
/// projected end state.
///
/// `validation.groups` starts with the incoming requirements and the affected
/// installed candidates, and grows as each resolved pass mentions new
/// capabilities. The first pass compiles only `Incoming`-owned residuals: the
/// relation plan that can remove an installed group's owner is only known after
/// a pass resolves. Every later pass compiles installed residuals whose owner is
/// not in the previous resolved pass's removal set.
///
/// Termination: every non-final pass promotes a root group to its unsimplified
/// expression, admits a candidate whose residual needs the solver, or turns a
/// violated non-root group into a root for the next pass. Promotion and
/// admission are one-way and finite. Rooting can repeat only when the relation
/// plan's removal set changes between passes, so the explicit pass counter (the
/// initial groups plus twice the candidate universe plus one) is the bound that
/// stops an oscillating removal set, returning the typed unsatisfied-groups
/// conflict.
pub(super) fn solve_validated_groups_to_fixed_point(
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
    // Pass one has no installed roots. Its resolved relation plan seeds the
    // exclusion set that decides which installed owners the next pass makes
    // into roots.
    let mut include_installed_roots = false;
    let mut excluded_trove_ids: HashSet<i64> = HashSet::new();
    loop {
        passes += 1;
        let roots = PassRoots {
            include_installed: include_installed_roots,
            excluded_trove_ids: &excluded_trove_ids,
        };
        let expressions =
            compile_group_residuals(&validation.groups, &validation.residuals, &roots)?;
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
                    &roots,
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

        // Promote a newly violated group to its unsimplified expression only
        // when this pass enforced it as a root: its simplified residual was
        // solved and the projection still violates it. A violated group that was
        // not a root (installed groups on the incoming-only first pass, or an
        // owner the previous pass removed) keeps its fixed-state residual and
        // becomes a root on the next pass if its owner survives this pass's
        // removal set. Promoting it early would discard the fixed-state
        // simplification, for example a condition only the incoming package
        // satisfies, which SAT cannot see. Groups already live and the residual
        // vector keep original group order, so every pass is deterministic.
        let next_excluded = removed_trove_ids(&remove_order);
        let next_roots = PassRoots {
            include_installed: true,
            excluded_trove_ids: &next_excluded,
        };
        let mut promoted = false;
        let mut newly_rooted = false;
        for &index in &violated {
            let group = &validation.groups[index];
            if !roots.includes(group) {
                newly_rooted |= next_roots.includes(group);
                continue;
            }
            if !validation.live[index] {
                validation.live[index] = true;
                validation.residuals[index] = Some(group.expression.clone());
                promoted = true;
            }
        }
        if !promoted && !added_unsatisfied && !newly_rooted {
            return Ok(unsatisfied_groups_conflict(
                &validation.groups,
                &validation.residuals,
                &violated,
            ));
        }
        if passes == validation.max_passes {
            return Ok(unsatisfied_groups_conflict(
                &validation.groups,
                &validation.residuals,
                &violated,
            ));
        }

        // The next pass makes every installed owner this pass did not remove
        // into a root, then revalidates against whatever removal set it plans.
        include_installed_roots = true;
        excluded_trove_ids = next_excluded;
    }
}

/// Compile the residual expression of each validated group that is a SAT root
/// for this pass, skipping discharged groups. The result preserves group order.
///
/// An `Incoming`-owned residual is always a root. An installed-owned residual
/// is a root only when `roots.include_installed` is set and its owning trove is
/// not in `roots.excluded_trove_ids`, the previous resolved pass's removal set.
fn compile_group_residuals(
    groups: &[ValidatedRequirementGroup],
    residuals: &[Option<RepositoryRequirementExpression>],
    roots: &PassRoots<'_>,
) -> Result<Vec<SolverExpression>> {
    groups
        .iter()
        .zip(residuals)
        .filter(|(group, _)| roots.includes(group))
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
            let relation_plan = plan_selected_relations(solver.provider(), &solvable_ids)?;
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
///
/// A pass that makes no progress always has a violated group to name. The
/// defensive pass-counter exit can be reached with none, in which case every
/// residual group is named so the refusal still carries typed owners.
fn unsatisfied_groups_conflict(
    groups: &[ValidatedRequirementGroup],
    residuals: &[Option<RepositoryRequirementExpression>],
    violated: &[usize],
) -> SatResolution {
    let indices = if violated.is_empty() {
        residuals
            .iter()
            .enumerate()
            .filter_map(|(index, residual)| residual.as_ref().map(|_| index))
            .collect::<Vec<_>>()
    } else {
        violated.to_vec()
    };
    let unsatisfied = indices
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
///
/// Only installed groups that were roots of the failing pass are attributed.
/// Pass one has no installed roots, and a later pass excludes the owners the
/// previous pass removed, so an owner that never made it into the failing
/// pass's root expressions is never named.
fn unsatisfiable_pass_resolution(
    conn: &Connection,
    validation: &EndStateValidation,
    solver_message: &str,
    policy: &ResolutionPolicy,
    outgoing_trove_ids: &[i64],
    lock_surviving_installed: bool,
    roots: &PassRoots<'_>,
) -> Result<SatResolution> {
    if !roots.include_installed {
        // The failing pass carried only incoming roots.
        return Ok(SatResolution::conflict(solver_message.to_string()));
    }

    // A pass without a resolution has no relation plan of its own. The root
    // exclusion that produced it is the previous pass's removal set, so that is
    // the set that decides which installed groups the pass itself required.
    let installed = validation
        .groups
        .iter()
        .zip(&validation.residuals)
        .filter(|(group, residual)| {
            residual.is_some()
                && matches!(group.owner, ValidatedGroupOwner::Installed { .. })
                && !group.owner_removed_by(roots.excluded_trove_ids)
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
