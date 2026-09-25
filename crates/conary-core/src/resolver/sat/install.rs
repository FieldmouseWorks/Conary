// crates/conary-core/src/resolver/sat/install.rs

use resolvo::{ConditionalRequirement, SolvableId};
use rusqlite::Connection;
use std::collections::HashSet;
use std::time::Instant;

use crate::error::Result;
use crate::repository::dependency_model::{
    RepositoryRequirementExpression, RepositoryRequirementGroup,
};
use crate::repository::resolution_policy::ResolutionPolicy;
use crate::repository::versioning::VersionScheme;
use crate::resolver::identity::PackageIdentity;
use crate::version::VersionConstraint;

use super::super::provider::{ConaryConstraint, ConaryProvider, SolverExpression};
use super::{SatPackage, SatRelationRemoval, SatSource, check_transitive_loading_limits, timing};

pub(super) fn build_provider_for_install<'conn>(
    conn: &'conn Connection,
    requests: &[(String, VersionConstraint)],
    policy: &ResolutionPolicy,
) -> Result<ConaryProvider<'conn>> {
    build_provider_for_install_ignoring_groups(conn, requests, policy, std::iter::empty())
}

pub(super) fn build_provider_for_install_ignoring_groups<'conn>(
    conn: &'conn Connection,
    requests: &[(String, VersionConstraint)],
    policy: &ResolutionPolicy,
    ignored: impl IntoIterator<
        Item = crate::resolver::provider::types::RepositoryRequirementGroupIdentity,
    >,
) -> Result<ConaryProvider<'conn>> {
    let phase = timing::start(None, timing::Phase::Initialization);
    let mut provider = ConaryProvider::new_with_policy(conn, policy.clone())?;
    drop(phase);
    #[cfg(test)]
    super::hidden_conflict::loaded_provider();
    provider.ignore_requirement_groups(ignored);
    provider.set_root_request_names(requests.iter().map(|(name, _)| name.clone()));
    let phase = timing::start(None, timing::Phase::Installed);
    provider.load_installed_packages()?;
    drop(phase);
    let phase = timing::start(None, timing::Phase::Canonical);
    provider.build_provides_index()?;
    provider.load_canonical_index()?;
    provider.expand_root_request_names_with_canonical_equivalents();
    drop(phase);
    let phase = timing::start(None, timing::Phase::Transitive);
    load_transitive_repo_packages(
        &mut provider,
        requests.iter().map(|(name, _)| name.clone()).collect(),
    )?;
    drop(phase);
    let phase = timing::start(None, timing::Phase::Compilation);
    provider.intern_all_dependency_version_sets()?;
    drop(phase);
    Ok(provider)
}

pub(super) fn build_provider_for_requirement_expressions<'conn>(
    conn: &'conn Connection,
    expressions: &[SolverExpression],
    policy: &ResolutionPolicy,
    outgoing_trove_ids: &[i64],
    lock_surviving_installed: bool,
) -> Result<ConaryProvider<'conn>> {
    let phase = timing::start(None, timing::Phase::Initialization);
    let mut provider = ConaryProvider::new_with_policy(conn, policy.clone())?;
    drop(phase);
    provider.set_root_request_names(requirement_names(expressions));
    provider.exclude_installed_troves(outgoing_trove_ids.iter().copied());
    if lock_surviving_installed {
        provider.lock_surviving_installed_candidates();
    }
    let phase = timing::start(None, timing::Phase::Installed);
    provider.load_installed_packages()?;
    drop(phase);
    let phase = timing::start(None, timing::Phase::Canonical);
    provider.build_provides_index()?;
    provider.load_canonical_index()?;
    provider.expand_root_request_names_with_canonical_equivalents();
    drop(phase);
    let phase = timing::start(None, timing::Phase::Transitive);
    load_transitive_repo_packages(&mut provider, requirement_names(expressions))?;
    drop(phase);
    let phase = timing::start(None, timing::Phase::Compilation);
    provider.intern_all_dependency_version_sets()?;
    drop(phase);
    Ok(provider)
}

fn load_transitive_repo_packages(
    provider: &mut ConaryProvider<'_>,
    mut loaded_names: HashSet<String>,
) -> Result<()> {
    let mut to_load: Vec<String> = loaded_names.iter().cloned().collect();
    let load_start = Instant::now();

    while !to_load.is_empty() {
        check_transitive_loading_limits(load_start.elapsed(), loaded_names.len())?;
        provider.load_repo_packages_for_names(&to_load)?;

        let mut new_names = provider
            .new_dependency_names(&loaded_names)
            .into_iter()
            .filter(|name| loaded_names.insert(name.clone()))
            .collect::<Vec<_>>();

        let canonical_equivalents = new_names
            .iter()
            .flat_map(|name| provider.canonical_equivalents(name).iter().cloned())
            .filter(|name| loaded_names.insert(name.clone()))
            .collect::<Vec<_>>();

        new_names.extend(canonical_equivalents);
        check_transitive_loading_limits(load_start.elapsed(), loaded_names.len())?;
        to_load = new_names;
    }

    tracing::debug!(
        target: "conary_core::resolver::timing",
        loaded_names = loaded_names.len(),
        admitted_candidates = provider.solvable_count(),
        "Resolver candidate discovery completed"
    );
    Ok(())
}

fn requirement_names(expressions: &[SolverExpression]) -> HashSet<String> {
    let known = HashSet::new();
    let mut names = HashSet::new();
    for expression in expressions {
        for atom in expression.positive_atoms() {
            match &atom.constraint {
                ConaryConstraint::ProviderExpression { expression } => {
                    expression.collect_names(&known, &mut names);
                }
                ConaryConstraint::RpmRuntime(_) => {}
                ConaryConstraint::ExactRepositoryPackage(_) => {}
                ConaryConstraint::Requested(_) | ConaryConstraint::Repository { .. } => {
                    names.insert(atom.name.clone());
                }
            }
        }
    }
    names
}

pub(super) fn build_requirements(
    provider: &mut ConaryProvider<'_>,
    requests: &[(String, VersionConstraint)],
) -> Result<Vec<ConditionalRequirement>> {
    let mut requirements = Vec::with_capacity(requests.len());

    for (name, constraint) in requests {
        let name_id = provider.intern_name(name)?;
        let version_set_id = provider.intern_version_set(name_id, constraint.clone())?;
        requirements.push(ConditionalRequirement::from(version_set_id));
    }

    Ok(requirements)
}

pub(super) fn build_expression_requirements(
    provider: &mut ConaryProvider<'_>,
    expressions: &[SolverExpression],
) -> Result<Vec<ConditionalRequirement>> {
    provider.compile_root_requirements(expressions)
}

/// The original hard groups evaluated against the transaction's fixed end
/// state.
pub(super) struct FixedStateEvaluation {
    /// `(installed - outgoing) + incoming`, package troves only.
    pub(super) end_state: Vec<PackageIdentity>,
    /// One residual per input hard group, in the same order.
    ///
    /// `None` means the fixed end state already satisfies (or vacuously
    /// discharges) the group, so the solver needs no repository work for it.
    pub(super) residuals: Vec<Option<RepositoryRequirementExpression>>,
}

/// Evaluate exact hard requirement groups against the transaction's fixed end
/// state, returning the fixed facts and the residual expressions that still
/// need repository work.
///
/// There are no choices to make against the fixed state, so the shared typed
/// expression evaluator decides each group directly. Groups that hold are
/// dropped. For the rest, [`simplify_against_end_state`] removes every
/// sub-expression the fixed state already satisfies. Residuals stay aligned
/// with the input groups so the caller can re-solve a violated group without
/// simplification.
pub(super) fn unsatisfied_groups_against_end_state(
    conn: &Connection,
    groups: &[&RepositoryRequirementGroup],
    version_scheme: VersionScheme,
    depending_architecture: &str,
    outgoing_trove_ids: &[i64],
    incoming: Option<&PackageIdentity>,
) -> Result<FixedStateEvaluation> {
    let end_state = fixed_end_state(conn, outgoing_trove_ids, incoming)?;
    let native_architecture = crate::repository::registry::detect_system_arch()?;

    let mut residuals = Vec::with_capacity(groups.len());
    for group in groups {
        if crate::resolver::requirement_expression_satisfied(
            &group.expression,
            version_scheme,
            depending_architecture,
            &native_architecture,
            &end_state,
        )? {
            residuals.push(None);
            continue;
        }
        residuals.push(simplify_against_end_state(
            &group.expression,
            version_scheme,
            depending_architecture,
            &native_architecture,
            &end_state,
        )?);
    }
    Ok(FixedStateEvaluation {
        end_state,
        residuals,
    })
}

/// Return the indices of original hard groups the projected end state does not
/// satisfy.
///
/// The projected end state is the fixed state plus every package SAT selected,
/// minus the exact installed troves the relation plan removes. Evaluating the
/// unsimplified groups against it catches a conditional the pre-solve
/// simplification dropped but that SAT then turned true by selecting the
/// condition package. The shared typed evaluator decides satisfaction, so the
/// check uses the same algebra as the fixed-state pass.
pub(super) fn groups_violated_by_solved_end_state(
    fixed_end_state: &[PackageIdentity],
    selected: &[PackageIdentity],
    remove_order: &[SatRelationRemoval],
    groups: &[&RepositoryRequirementGroup],
    version_scheme: VersionScheme,
    depending_architecture: &str,
    native_architecture: &str,
) -> Result<Vec<usize>> {
    let mut projected = fixed_end_state.to_vec();
    if !remove_order.is_empty() {
        let removed = remove_order
            .iter()
            .map(|removal| removal.trove_id)
            .collect::<HashSet<_>>();
        projected.retain(|package| {
            !package
                .installed_trove_id
                .is_some_and(|trove_id| removed.contains(&trove_id))
        });
    }
    projected.extend(selected.iter().cloned());

    let mut violated = Vec::new();
    for (index, group) in groups.iter().enumerate() {
        if !crate::resolver::requirement_expression_satisfied(
            &group.expression,
            version_scheme,
            depending_architecture,
            native_architecture,
            &projected,
        )? {
            violated.push(index);
        }
    }
    Ok(violated)
}

/// The exact package identities resolvo selected for the solve.
pub(super) fn collect_selected_identities(
    provider: &ConaryProvider<'_>,
    solvable_ids: &[SolvableId],
) -> Vec<PackageIdentity> {
    solvable_ids
        .iter()
        .map(|solvable_id| provider.get_solvable(*solvable_id).clone())
        .collect()
}

/// The transaction's fixed end state: every installed package trove except
/// `outgoing_trove_ids`, plus `incoming`.
///
/// Only package-type troves are facts. Collections created by
/// `conary collection create` have no architecture, so including them makes the
/// typed evaluator reject the whole set instead of evaluating the group.
fn fixed_end_state(
    conn: &Connection,
    outgoing_trove_ids: &[i64],
    incoming: Option<&PackageIdentity>,
) -> Result<Vec<PackageIdentity>> {
    let mut end_state =
        crate::resolver::requirements::load_installed_package_identities_for_packages(conn)?;
    if !outgoing_trove_ids.is_empty() {
        let outgoing_trove_ids = outgoing_trove_ids.iter().copied().collect::<HashSet<_>>();
        end_state.retain(|package| {
            !package
                .installed_trove_id
                .is_some_and(|trove_id| outgoing_trove_ids.contains(&trove_id))
        });
    }
    if let Some(incoming) = incoming {
        end_state.push(incoming.clone());
    }
    Ok(end_state)
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
) -> Result<Option<RepositoryRequirementExpression>> {
    use RepositoryRequirementExpression as Expression;

    match expression {
        // A capability or same-provider expression the fixed end state already
        // satisfies is true for the whole transaction and needs no repository
        // work. Composite nodes decide their own satisfaction structurally, so a
        // sub-expression is evaluated exactly once.
        Expression::Atom(_) | Expression::With { .. } | Expression::Without { .. } => {
            if crate::resolver::requirement_expression_satisfied(
                expression,
                version_scheme,
                depending_architecture,
                native_architecture,
                end_state,
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
) -> Result<bool> {
    crate::resolver::requirement_expression_satisfied(
        condition,
        version_scheme,
        depending_architecture,
        native_architecture,
        end_state,
    )
}

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
