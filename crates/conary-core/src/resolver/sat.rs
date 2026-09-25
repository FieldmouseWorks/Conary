// crates/conary-core/src/resolver/sat.rs

//! SAT-based dependency resolution using resolvo.
//!
//! Provides policy-explicit install solving and exact removal analysis using
//! the CDCL SAT solver with backtracking support.

mod hidden_conflict;
mod install;
mod relations;
mod removal;
mod timing;

use resolvo::{Problem, Solver, UnsolvableOrCancelled};
use rusqlite::Connection;
use std::collections::BTreeSet;
use std::time::Duration;

use petgraph::Direction;
use petgraph::visit::EdgeRef;

use crate::error::{Error, Result};
use crate::packages::PackageFormat;
use crate::repository::dependency_model::{
    ProvidedCapability, RepositoryRequirementExpression, RepositoryRequirementGroup,
    RepositoryRequirementKind,
};
use crate::repository::resolution_policy::ResolutionPolicy;
use crate::repository::versioning::VersionScheme;
use crate::resolver::identity::PackageIdentity;
use crate::resolver::provider::SolverExpression;
use crate::version::VersionConstraint;

const MAX_LOADED_NAMES: usize = 50_000;
const TRANSITIVE_LOAD_TIMEOUT: Duration = Duration::from_secs(30);

fn check_transitive_loading_limits(elapsed: Duration, loaded_names: usize) -> Result<()> {
    if loaded_names > MAX_LOADED_NAMES {
        return Err(Error::InitError(format!(
            "Dependency resolution discovered too many dependency names ({loaded_names} > {MAX_LOADED_NAMES})"
        )));
    }

    if elapsed > TRANSITIVE_LOAD_TIMEOUT {
        return Err(Error::InitError(format!(
            "Dependency resolution timed out while loading transitive dependencies after {:?}",
            elapsed
        )));
    }

    Ok(())
}

/// Source of a resolved package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SatSource {
    /// Package is already installed on the system.
    Installed,
    /// Package comes from a repository.
    Repository,
}

/// A single package in the SAT resolution result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SatPackage {
    pub name: String,
    pub version: String,
    pub package_release: Option<String>,
    pub architecture: Option<String>,
    pub version_scheme: crate::repository::versioning::VersionScheme,
    pub repo_package_id: Option<i64>,
    pub repository_id: Option<i64>,
    pub repository_name: Option<String>,
    pub installed_trove_id: Option<i64>,
    pub source: SatSource,
}

/// Installed package removal authorized by exact selected relation facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SatRelationRemoval {
    pub trove_id: i64,
    pub package: SatPackage,
    /// Exact selected packages whose combined facts make the relation apply.
    pub authorized_by: Vec<SatPackage>,
    pub kind: crate::repository::dependency_model::RepositoryRequirementKind,
    pub mode: crate::repository::dependency_model::PackageRelationRemovalMode,
    pub native_text: Option<String>,
}

/// Result of SAT-based dependency resolution.
#[derive(Debug)]
pub struct SatResolution {
    /// Packages to install/upgrade, in dependency order.
    pub install_order: Vec<SatPackage>,
    /// Explicit replacement/obsolescence removals, never inferred from a
    /// positive dependency or provide.
    pub remove_order: Vec<SatRelationRemoval>,
    /// Human-readable conflict explanation if unsolvable.
    pub conflict_message: Option<String>,
}

impl SatResolution {
    /// A resolution that selects and installs nothing.
    fn empty() -> Self {
        Self {
            install_order: Vec::new(),
            remove_order: Vec::new(),
            conflict_message: None,
        }
    }

    /// A successful resolution with its exact install and removal orders.
    fn resolved(install_order: Vec<SatPackage>, remove_order: Vec<SatRelationRemoval>) -> Self {
        Self {
            install_order,
            remove_order,
            conflict_message: None,
        }
    }

    /// A resolution that refuses the request with a typed conflict explanation.
    fn conflict(message: String) -> Self {
        Self {
            install_order: Vec::new(),
            remove_order: Vec::new(),
            conflict_message: Some(message),
        }
    }
}

/// One exact required group that has no candidate provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SatUnresolvedDependency {
    pub repository_package_id: i64,
    pub repository_requirement_group_id: i64,
}

/// Typed result for one exact repository-package root against empty state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SatExactResolution {
    Resolved {
        install_order: Vec<SatPackage>,
    },
    Unresolved {
        dependencies: Vec<SatUnresolvedDependency>,
    },
    ConflictingClosure,
}

fn conflict_graph_has_conflict_class(
    graph: &resolvo::conflict::ConflictGraph<resolvo::SolvableId>,
) -> bool {
    graph
        .graph
        .edge_references()
        .any(|edge| matches!(edge.weight(), resolvo::conflict::ConflictEdge::Conflict(_)))
        || graph
            .graph
            .node_weights()
            .any(|node| matches!(node, resolvo::conflict::ConflictNode::Excluded(_)))
}

/// Solve an install request using the SAT solver with an explicit source-selection policy.
pub fn solve_install_with_policy(
    conn: &Connection,
    requests: &[(String, VersionConstraint)],
    policy: &ResolutionPolicy,
) -> Result<SatResolution> {
    if requests.is_empty() {
        return Ok(SatResolution {
            install_order: Vec::new(),
            remove_order: Vec::new(),
            conflict_message: None,
        });
    }
    policy
        .validate_for_dependency_resolution()
        .map_err(Error::ConfigError)?;

    let mut provider = install::build_provider_for_install(conn, requests, policy)?;
    let requirements = install::build_requirements(&mut provider, requests)?;

    let problem = Problem::new().requirements(requirements);

    // Solve
    let mut solver = Solver::new(provider);
    match solver.solve(problem) {
        Ok(solvable_ids) => {
            let relation_plan =
                relations::plan_selected_relations(solver.provider(), &solvable_ids)?;
            Ok(SatResolution {
                install_order: if relation_plan.conflict.is_some() {
                    Vec::new()
                } else {
                    install::collect_install_order(solver.provider(), &solvable_ids)
                },
                remove_order: relation_plan.removals,
                conflict_message: relation_plan.conflict,
            })
        }
        Err(UnsolvableOrCancelled::Unsolvable(conflict)) => {
            let message = conflict.display_user_friendly(&solver).to_string();
            Ok(SatResolution {
                install_order: Vec::new(),
                remove_order: Vec::new(),
                conflict_message: Some(message),
            })
        }
        Err(UnsolvableOrCancelled::Cancelled(_)) => Err(Error::InitError(
            "Dependency resolution was cancelled".to_string(),
        )),
    }
}

/// Resolve one exact persisted repository package for a target architecture.
///
/// Unlike the user-facing name/version request, this root constraint cannot
/// select a different release, architecture, repository, or package variant.
/// Missing positive dependency groups are returned as persisted typed
/// authority; root-reachable conflicts are a separate typed outcome and only
/// unattributed solver failures remain hard errors.
pub fn solve_exact_repository_package_with_policy(
    conn: &Connection,
    repository_package_id: i64,
    architecture: &str,
    policy: &ResolutionPolicy,
) -> Result<SatExactResolution> {
    solve_exact_repository_package_with_policy_inner(
        conn,
        repository_package_id,
        architecture,
        policy,
        |_, _| {},
    )
}

/// Resolve one exact package while exposing typed resolvo conflict data only
/// when the exact-root projection fails.
pub(crate) fn solve_exact_repository_package_with_policy_and_failure_graph(
    conn: &Connection,
    repository_package_id: i64,
    architecture: &str,
    policy: &ResolutionPolicy,
    on_failure: impl FnMut(
        &resolvo::conflict::ConflictGraph<resolvo::SolvableId>,
        &crate::resolver::provider::ConaryProvider<'_>,
    ),
) -> Result<SatExactResolution> {
    solve_exact_repository_package_with_policy_inner(
        conn,
        repository_package_id,
        architecture,
        policy,
        on_failure,
    )
}

fn solve_exact_repository_package_with_policy_inner(
    conn: &Connection,
    repository_package_id: i64,
    architecture: &str,
    policy: &ResolutionPolicy,
    mut on_failure: impl FnMut(
        &resolvo::conflict::ConflictGraph<resolvo::SolvableId>,
        &crate::resolver::provider::ConaryProvider<'_>,
    ),
) -> Result<SatExactResolution> {
    #[cfg(test)]
    hidden_conflict::reset_counts();
    let preparation = timing::start(repository_package_id, timing::Phase::Preparation);
    let root = crate::db::models::RepositoryPackage::find_by_id(conn, repository_package_id)?
        .ok_or_else(|| {
            Error::NotFound(format!(
                "repository package {repository_package_id} does not exist"
            ))
        })?;
    policy
        .validate_for_dependency_resolution()
        .map_err(Error::ConfigError)?;

    let requests = vec![(root.name.clone(), VersionConstraint::Any)];
    let mut provider = install::build_provider_for_install(conn, &requests, policy)?;
    provider.set_native_architecture(architecture);
    let exact = provider.intern_exact_repository_package(&root.name, repository_package_id)?;
    let problem = Problem::new().requirements(vec![exact.into()]);
    let mut solver = Solver::new(provider);

    drop(preparation);
    let solving = timing::start(repository_package_id, timing::Phase::Solve);
    let result = solver.solve(problem);
    drop(solving);
    let _classification = timing::start(repository_package_id, timing::Phase::Classification);
    match result {
        Ok(solvable_ids) => {
            let relation_plan =
                relations::plan_selected_relations(solver.provider(), &solvable_ids)?;
            if let Some(conflict) = relation_plan.conflict {
                return Err(Error::ConflictError(format!(
                    "exact repository package {repository_package_id} conflicts under empty installed state: {conflict}"
                )));
            }
            if !relation_plan.removals.is_empty() {
                return Err(Error::InternalError(format!(
                    "exact repository package {repository_package_id} planned removals against empty installed state"
                )));
            }
            Ok(SatExactResolution::Resolved {
                install_order: install::collect_install_order(solver.provider(), &solvable_ids),
            })
        }
        Err(UnsolvableOrCancelled::Unsolvable(conflict)) => {
            let graph = conflict.graph(&solver);
            let projected = (|| {
                let has_conflict_class = conflict_graph_has_conflict_class(&graph);
                let Some(unresolved) = graph.unresolved_node else {
                    if has_conflict_class {
                        return Ok(SatExactResolution::ConflictingClosure);
                    }
                    return Err(Error::ConflictError(format!(
                        "exact repository package {repository_package_id} is unsatisfiable without a missing typed dependency: {}",
                        conflict.display_user_friendly(&solver)
                    )));
                };
                let mut dependencies = BTreeSet::new();
                for edge in graph.graph.edges_directed(unresolved, Direction::Incoming) {
                    let resolvo::conflict::ConflictEdge::Requires(requirement) = *edge.weight()
                    else {
                        if has_conflict_class {
                            return Ok(SatExactResolution::ConflictingClosure);
                        }
                        return Err(Error::InternalError(
                            "resolver unresolved sink has a non-requirement edge".to_string(),
                        ));
                    };
                    let resolvo::conflict::ConflictNode::Solvable(requiring) =
                        graph.graph[edge.source()]
                    else {
                        return Err(Error::ConflictError(format!(
                            "exact repository package {repository_package_id} root constraint has no eligible candidate for architecture '{architecture}'"
                        )));
                    };
                    for group in solver
                        .provider()
                        .unresolved_requirement_groups(requiring, requirement)
                    {
                        dependencies.insert(SatUnresolvedDependency {
                            repository_package_id: group.repository_package_id,
                            repository_requirement_group_id: group.repository_requirement_group_id,
                        });
                    }
                }
                if dependencies.is_empty() {
                    if has_conflict_class {
                        return Ok(SatExactResolution::ConflictingClosure);
                    }
                    return Err(Error::ConflictError(format!(
                        "exact repository package {repository_package_id} is unresolved without a persisted required-group authority"
                    )));
                }
                let dependencies = dependencies.into_iter().collect::<Vec<_>>();
                let Some(dependencies) = hidden_conflict::probe(
                    conn,
                    &root.name,
                    repository_package_id,
                    architecture,
                    policy,
                    &dependencies,
                )?
                else {
                    return Ok(SatExactResolution::ConflictingClosure);
                };
                Ok(SatExactResolution::Unresolved { dependencies })
            })();
            if projected.is_err() {
                on_failure(&graph, solver.provider());
            }
            projected
        }
        Err(UnsolvableOrCancelled::Cancelled(_)) => Err(Error::InitError(
            "Dependency resolution was cancelled".to_string(),
        )),
    }
}

/// Whether the caller knows the exact installed troves its transaction removes.
///
/// The end state of a transaction is `(installed - outgoing) + incoming`. A
/// caller that has not computed its outgoing set cannot be answered from
/// installed state alone: an installed provider may be removed after the solve,
/// so approving the requirement would be unsound.
#[derive(Debug, Clone, Copy)]
enum EndState<'a> {
    /// The caller knows every installed trove the transaction removes.
    Known { outgoing_trove_ids: &'a [i64] },
    /// The caller has not computed its outgoing set.
    Unknown,
}

/// Solve exact typed package requirements using their source-native version
/// algebra and Boolean expression semantics.
///
/// The transaction's end state is unknown: the caller has not supplied the
/// exact installed troves it removes. Under strict mixing with no repository
/// authority the solve refuses rather than satisfying the requirements from
/// installed state that the transaction may later remove. Callers that know
/// their outgoing set use
/// [`solve_requirement_groups_with_outgoing_and_policy`].
pub fn solve_requirement_groups_with_policy(
    conn: &Connection,
    groups: &[RepositoryRequirementGroup],
    version_scheme: VersionScheme,
    policy: &ResolutionPolicy,
) -> Result<SatResolution> {
    let depending_architecture =
        crate::repository::registry::native_architecture_for_scheme(version_scheme)?;
    solve_requirement_groups_for_architecture_with_policy(
        conn,
        groups,
        version_scheme,
        &depending_architecture,
        EndState::Unknown,
        None,
        policy,
    )
}

/// Solve exact typed package requirements against the transaction's end state.
///
/// `outgoing_trove_ids` are exact installed trove identities the owning
/// transaction removes. They are excluded from installed candidates so a
/// requirement is never satisfied by a package that will not exist afterwards.
/// Unlike [`solve_requirement_groups_with_policy`] this is a known end state, so
/// strict mixing with no repository authority may be discharged against the
/// fixed `(installed - outgoing)` set.
pub fn solve_requirement_groups_with_outgoing_and_policy(
    conn: &Connection,
    groups: &[RepositoryRequirementGroup],
    version_scheme: VersionScheme,
    outgoing_trove_ids: &[i64],
    policy: &ResolutionPolicy,
) -> Result<SatResolution> {
    let depending_architecture =
        crate::repository::registry::native_architecture_for_scheme(version_scheme)?;
    solve_requirement_groups_for_architecture_with_policy(
        conn,
        groups,
        version_scheme,
        &depending_architecture,
        EndState::Known { outgoing_trove_ids },
        None,
        policy,
    )
}

fn solve_requirement_groups_for_architecture_with_policy(
    conn: &Connection,
    groups: &[RepositoryRequirementGroup],
    version_scheme: VersionScheme,
    depending_architecture: &str,
    end_state: EndState<'_>,
    incoming: Option<&PackageIdentity>,
    policy: &ResolutionPolicy,
) -> Result<SatResolution> {
    let mut hard_groups = Vec::new();
    for group in groups {
        crate::repository::requirement::validate_requirement_group(group, version_scheme)
            .map_err(Error::ConfigError)?;
        match group.kind {
            RepositoryRequirementKind::Depends | RepositoryRequirementKind::PreDepends => {
                hard_groups.push(group);
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

    if hard_groups.is_empty() {
        return Ok(SatResolution::empty());
    }

    // A malformed source identity is always a hard error; installed
    // satisfaction never repairs an invalid policy.
    policy
        .validate_source_identities()
        .map_err(Error::ConfigError)?;
    let invalid_policy = policy.validate_for_dependency_resolution().err();
    let native_architecture = crate::repository::registry::detect_system_arch()?;

    let outgoing_trove_ids: &[i64] = match end_state {
        EndState::Known { outgoing_trove_ids } => outgoing_trove_ids,
        EndState::Unknown => &[],
    };
    let lock_surviving_installed = matches!(end_state, EndState::Known { .. });

    // The transaction's end state is fixed: every installed package trove except
    // the outgoing set, plus the incoming package. A hard group that end state
    // already satisfies needs no repository work, so only its residual groups
    // reach SAT. Their conditions are resolved against the fixed state first so
    // the solver cannot drop a surviving provider to discharge the group
    // vacuously. Residuals stay aligned with `hard_groups` so a group the
    // pre-solve simplification distorted can be retried unsimplified.
    let (fixed_end_state, group_residuals) = match end_state {
        EndState::Unknown => {
            if let Some(message) = invalid_policy.as_deref() {
                return Err(Error::ConfigError(message.to_string()));
            }
            (
                None,
                hard_groups
                    .iter()
                    .map(|group| Some(group.expression.clone()))
                    .collect::<Vec<_>>(),
            )
        }
        EndState::Known { outgoing_trove_ids } => {
            let evaluated = install::unsatisfied_groups_against_end_state(
                conn,
                &hard_groups,
                version_scheme,
                depending_architecture,
                outgoing_trove_ids,
                incoming,
            )?;
            if evaluated.residuals.iter().all(Option::is_none) {
                return Ok(SatResolution::empty());
            }
            if let Some(message) = invalid_policy.as_deref() {
                // Strict mixing with no repository authority admits only the
                // fixed end state itself.
                return Err(Error::ConfigError(message.to_string()));
            }
            (Some(evaluated.end_state), evaluated.residuals)
        }
    };

    // The fixed-state simplification only decides the conditions it can see.
    // SAT can select a condition package and make another conditional live,
    // which can make yet another one live. Iterate passes until every original
    // hard group holds against the full projected end state, promoting every
    // newly violated group to its unsimplified expression so resolvo sees the
    // live conditional rather than the fixed-state decision that dropped it.
    //
    // Each pass either succeeds, stops with a typed conflict, or promotes at
    // least one newly violated group. A group is promoted at most once and the
    // live set is carried across passes, so the live set strictly grows until a
    // pass promotes nothing; that pass returns the conflict. At most
    // `hard_groups.len()` passes promote, so `hard_groups.len() + 1` passes
    // bound the loop. The counter makes the bound explicit and stops a logic
    // error from looping forever.
    let max_passes = hard_groups.len() + 1;
    let mut residuals = group_residuals;
    let mut live = vec![false; hard_groups.len()];
    let mut passes = 0;
    loop {
        passes += 1;
        let expressions =
            compile_group_residuals(&residuals, version_scheme, depending_architecture)?;
        let pass = solve_expression_pass(
            conn,
            &expressions,
            policy,
            outgoing_trove_ids,
            lock_surviving_installed,
        )?;

        let (install_order, remove_order, selected) = match pass {
            ExpressionPass::Conflict(message) => return Ok(SatResolution::conflict(message)),
            ExpressionPass::Resolved {
                install_order,
                remove_order,
                selected,
            } => (install_order, remove_order, selected),
        };

        // An unknown end state cannot be projected, so the caller's own
        // semantics apply and there is nothing to validate against.
        let Some(fixed_end_state) = fixed_end_state.as_ref() else {
            return Ok(SatResolution::resolved(install_order, remove_order));
        };

        let violated = install::groups_violated_by_solved_end_state(
            fixed_end_state,
            &selected,
            &remove_order,
            &hard_groups,
            version_scheme,
            depending_architecture,
            &native_architecture,
        )?;
        if violated.is_empty() {
            return Ok(SatResolution::resolved(install_order, remove_order));
        }

        // Promote every newly violated group to its unsimplified expression.
        // Groups already live and the residual vector keep original group
        // order, so every pass is deterministic.
        let mut promoted = false;
        for &index in &violated {
            if !live[index] {
                live[index] = true;
                residuals[index] = Some(hard_groups[index].expression.clone());
                promoted = true;
            }
        }
        if !promoted || passes == max_passes {
            return Ok(SatResolution::conflict(unsatisfied_groups_message(
                &hard_groups,
                &violated,
            )));
        }
    }
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

/// Compile the residual expression of each hard group, skipping discharged
/// groups. The result preserves group order.
fn compile_group_residuals(
    residuals: &[Option<RepositoryRequirementExpression>],
    version_scheme: VersionScheme,
    depending_architecture: &str,
) -> Result<Vec<SolverExpression>> {
    residuals
        .iter()
        .flatten()
        .map(|expression| {
            crate::resolver::provider::repository_expression_to_solver_for_architecture(
                expression,
                version_scheme,
                depending_architecture,
            )
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
    let mut provider = install::build_provider_for_requirement_expressions(
        conn,
        expressions,
        policy,
        outgoing_trove_ids,
        lock_surviving_installed,
    )?;
    let requirements = install::build_expression_requirements(&mut provider, expressions)?;
    let problem = Problem::new().requirements(requirements);
    let mut solver = Solver::new(provider);
    match solver.solve(problem) {
        Ok(solvable_ids) => {
            let relation_plan =
                relations::plan_selected_relations(solver.provider(), &solvable_ids)?;
            if let Some(conflict) = relation_plan.conflict {
                return Ok(ExpressionPass::Conflict(conflict));
            }
            Ok(ExpressionPass::Resolved {
                install_order: install::collect_install_order(solver.provider(), &solvable_ids),
                remove_order: relation_plan.removals,
                selected: install::collect_selected_identities(solver.provider(), &solvable_ids),
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

/// A conflict explanation naming every hard group the fixed-point iteration
/// could not place in the projected end state, as decided by the shared typed
/// evaluator.
fn unsatisfied_groups_message(
    hard_groups: &[&RepositoryRequirementGroup],
    violated: &[usize],
) -> String {
    let groups = violated
        .iter()
        .map(|&index| {
            let group = hard_groups[index];
            group
                .native_text
                .as_deref()
                .filter(|text| !text.trim().is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("{:?}", group.expression))
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "the solved install order leaves hard requirement group(s) unsatisfied in the transaction end state: {groups}"
    )
}

/// Return whether an incoming package already satisfies one positive
/// requirement group from its exact identity and declared provides.
///
/// Conditional and negated forms are left to the end-state evaluator because
/// their truth can depend on other packages. Positive atoms, conjunctions, and
/// disjunctions are safe to discharge against the incoming package alone.
pub fn positive_requirement_group_satisfied_by_package(
    group: &RepositoryRequirementGroup,
    version_scheme: VersionScheme,
    package: &PackageIdentity,
) -> Result<bool> {
    crate::repository::requirement::validate_requirement_group(group, version_scheme)
        .map_err(Error::ConfigError)?;
    if !matches!(
        group.kind,
        RepositoryRequirementKind::Depends | RepositoryRequirementKind::PreDepends
    ) {
        return Ok(false);
    }

    fn matches_positive_expression(
        expression: &crate::repository::dependency_model::RepositoryRequirementExpression,
        version_scheme: VersionScheme,
        package: &PackageIdentity,
        depending_architecture: &str,
        native_architecture: &str,
    ) -> Result<bool> {
        use crate::repository::dependency_model::RepositoryRequirementExpression as Expression;
        match expression {
            Expression::Atom(_) => {
                let solver_expression =
                    crate::resolver::provider::repository_expression_to_solver_for_architecture(
                        expression,
                        version_scheme,
                        depending_architecture,
                    )?;
                let crate::resolver::provider::SolverExpression::Atom(atom) = solver_expression
                else {
                    return Ok(false);
                };
                crate::resolver::provider::matching::constraint_matches_candidate(
                    &atom.name,
                    &atom.constraint,
                    package,
                    native_architecture,
                    package.name == atom.name,
                )
            }
            Expression::And(operands) => {
                for operand in operands {
                    if !matches_positive_expression(
                        operand,
                        version_scheme,
                        package,
                        depending_architecture,
                        native_architecture,
                    )? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            Expression::Or(operands) => {
                for operand in operands {
                    if matches_positive_expression(
                        operand,
                        version_scheme,
                        package,
                        depending_architecture,
                        native_architecture,
                    )? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            Expression::If { .. }
            | Expression::Unless { .. }
            | Expression::With { .. }
            | Expression::Without { .. } => Ok(false),
        }
    }

    let native_architecture = crate::repository::registry::detect_system_arch()?;
    let depending_architecture = package
        .architecture
        .as_deref()
        .unwrap_or(native_architecture.as_str());
    matches_positive_expression(
        &group.expression,
        version_scheme,
        package,
        depending_architecture,
        &native_architecture,
    )
}

/// Solve one parsed package's external requirements after discharging exact
/// positive requirements that the given provided capabilities cover.
///
/// The transaction's end state is unknown: the caller has not supplied the
/// exact installed troves it removes. Under strict mixing with no repository
/// authority the solve refuses rather than satisfying a requirement from an
/// installed provider the transaction may later remove. Callers that know
/// their outgoing set use
/// [`solve_package_requirements_with_provides_outgoing_and_policy`].
///
/// Callers that have already reduced `package.resolution_capabilities()` to the
/// exact set their selection installs pass that view here, so a requirement is
/// never discharged against a provide the selected payload does not ship.
pub fn solve_package_requirements_with_provides_and_policy(
    conn: &Connection,
    package: &dyn PackageFormat,
    provided_capabilities: Vec<ProvidedCapability>,
    policy: &ResolutionPolicy,
) -> Result<SatResolution> {
    solve_package_requirements_with_provides_for_end_state(
        conn,
        package,
        provided_capabilities,
        EndState::Unknown,
        policy,
    )
}

/// Solve one parsed package's external requirements against the transaction's
/// end state after discharging exact positive requirements that the given
/// provided capabilities cover.
///
/// `outgoing_trove_ids` are exact installed troves the owning transaction
/// removes. They are excluded from the installed solve so a requirement is
/// never satisfied by a package that will not exist afterwards. Because the end
/// state is known, strict mixing with no repository authority is discharged
/// against the fixed `(installed - outgoing) + incoming` set, which includes the
/// incoming package's own provided capabilities.
pub fn solve_package_requirements_with_provides_outgoing_and_policy(
    conn: &Connection,
    package: &dyn PackageFormat,
    provided_capabilities: Vec<ProvidedCapability>,
    outgoing_trove_ids: &[i64],
    policy: &ResolutionPolicy,
) -> Result<SatResolution> {
    solve_package_requirements_with_provides_for_end_state(
        conn,
        package,
        provided_capabilities,
        EndState::Known { outgoing_trove_ids },
        policy,
    )
}

fn solve_package_requirements_with_provides_for_end_state(
    conn: &Connection,
    package: &dyn PackageFormat,
    provided_capabilities: Vec<ProvidedCapability>,
    end_state: EndState<'_>,
    policy: &ResolutionPolicy,
) -> Result<SatResolution> {
    let incoming = PackageIdentity {
        repo_package_id: None,
        name: package.name().to_string(),
        version: package.version().to_string(),
        package_release: package.package_release().map(str::to_string),
        architecture: package.architecture().map(str::to_string),
        debian_multi_arch: package.debian_multi_arch(),
        version_scheme: package.version_scheme(),
        repository_id: None,
        repository_name: String::new(),
        repository_profile: None,
        repository_priority: 0,
        canonical_id: None,
        canonical_name: None,
        installed_trove_id: None,
        installed_pinned: false,
        provided_capabilities,
    };
    let mut external_requirements = Vec::new();
    for requirement in package.requirements() {
        if !positive_requirement_group_satisfied_by_package(
            requirement,
            package.version_scheme(),
            &incoming,
        )? {
            external_requirements.push(requirement.clone());
        }
    }
    let depending_architecture = match package.architecture() {
        Some(architecture) => architecture.to_string(),
        None => {
            crate::repository::registry::native_architecture_for_scheme(package.version_scheme())?
        }
    };
    solve_requirement_groups_for_architecture_with_policy(
        conn,
        &external_requirements,
        package.version_scheme(),
        &depending_architecture,
        end_state,
        Some(&incoming),
        policy,
    )
}

/// Solve one parsed package's external requirements after discharging exact
/// positive requirements that the incoming package itself provides.
///
/// All install entrypoints without a component selection use this boundary so a
/// converted CCS archive and its source-native package receive identical
/// dependency semantics. The transaction's end state is unknown because the
/// caller has not yet computed the installed troves it removes; strict mixing
/// with no repository authority therefore refuses instead of trusting installed
/// state.
pub fn solve_package_requirements_with_policy(
    conn: &Connection,
    package: &dyn PackageFormat,
    policy: &ResolutionPolicy,
) -> Result<SatResolution> {
    solve_package_requirements_with_provides_and_policy(
        conn,
        package,
        package.resolution_capabilities()?,
        policy,
    )
}

/// Check what packages would break if the given packages are removed.
///
/// Returns the names of packages whose dependencies would be unsatisfied.
///
/// Instead of BFS on package names (which mishandles OR-deps and virtual
/// provides), this evaluates each dependent's full dependency clause set.
/// For OR-deps, a clause is only broken when ALL alternatives are gone.
/// The analysis iterates to a fixed point: breaking one package may cause
/// others to lose a provider, so we re-evaluate until no new breakage is found.
pub fn solve_removal(conn: &Connection, to_remove: &[String]) -> Result<Vec<String>> {
    let provider = removal::build_provider_for_removal(conn)?;
    removal::find_breaking_packages(&provider, to_remove)
}

/// Check dependency breakage for exact installed package identities.
///
/// Unlike [`solve_removal`], this preserves co-installed instances of the same
/// package name and removes only the supplied trove IDs.
pub fn solve_removal_troves(conn: &Connection, to_remove: &[i64]) -> Result<Vec<String>> {
    let provider = removal::build_provider_for_removal(conn)?;
    removal::find_breaking_packages_for_troves(&provider, to_remove)
}

#[cfg(test)]
#[path = "sat/tests.rs"]
mod tests;
