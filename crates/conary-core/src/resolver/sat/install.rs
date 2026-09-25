// crates/conary-core/src/resolver/sat/install.rs

use resolvo::ConditionalRequirement;
use rusqlite::Connection;
use std::collections::HashSet;
use std::time::Instant;

use crate::error::Result;
use crate::repository::resolution_policy::ResolutionPolicy;
use crate::resolver::identity::PackageIdentity;
use crate::version::VersionConstraint;

use super::super::provider::types::RequirementGroupIdentity;
use super::super::provider::{ConaryConstraint, ConaryProvider, SolverExpression};
use super::{check_transitive_loading_limits, timing};

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
    ignored: impl IntoIterator<Item = RequirementGroupIdentity>,
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
    incoming: Option<&PackageIdentity>,
    facts: FixedTransactionFacts<'_>,
) -> Result<ConaryProvider<'conn>> {
    let FixedTransactionFacts {
        outgoing_trove_ids,
        relation_only_trove_ids,
        lock_surviving_installed,
        ignored_installed_groups,
    } = facts;
    let phase = timing::start(None, timing::Phase::Initialization);
    let mut provider = ConaryProvider::new_with_policy(conn, policy.clone())?;
    drop(phase);
    provider.set_root_request_names(requirement_names(expressions));
    // Caller-declared outgoing troves are not part of the end state at all.
    // Relation-removed troves from an earlier pass stay loaded (relation
    // planning re-derives each pass's exact removal set) but are hidden from
    // candidate discovery.
    provider.exclude_installed_troves(outgoing_trove_ids.iter().copied());
    provider.hide_relation_only_installed_troves(relation_only_trove_ids.iter().copied());
    if lock_surviving_installed {
        provider.lock_surviving_installed_candidates();
    }
    let phase = timing::start(None, timing::Phase::Installed);
    provider.load_installed_packages()?;
    if let Some(incoming) = incoming {
        provider.add_fixed_incoming(incoming.clone())?;
    }
    drop(phase);
    let phase = timing::start(None, timing::Phase::Canonical);
    provider.build_provides_index()?;
    provider.load_canonical_index()?;
    provider.expand_root_request_names_with_canonical_equivalents();
    // A forced installed package's stored hard groups are enforced natively by
    // SAT. Groups already unsatisfied before the transaction are discharged by
    // identity so pre-existing breakage never makes the solve unsatisfiable.
    // Discharging after canonical loading keeps the recompiled condition set
    // consistent with the final compilation.
    if !ignored_installed_groups.is_empty() {
        provider.discharge_requirement_groups(ignored_installed_groups.iter().copied())?;
    }
    drop(phase);
    let phase = timing::start(None, timing::Phase::Transitive);
    load_transitive_repo_packages(&mut provider, requirement_names(expressions))?;
    drop(phase);
    let phase = timing::start(None, timing::Phase::Compilation);
    provider.intern_all_dependency_version_sets()?;
    // Replacement exclusions name the loaded candidate pool, so they compile
    // only once discovery is complete.
    provider.compile_replacement_constrains()?;
    drop(phase);
    Ok(provider)
}

/// The fixed-transaction facts a requirement-expression provider must honor.
pub(super) struct FixedTransactionFacts<'a> {
    /// Exact installed trove IDs the owning transaction removes entirely.
    pub(super) outgoing_trove_ids: &'a [i64],
    /// Installed troves an earlier pass's relation plan removes; loaded but
    /// hidden from candidate discovery.
    pub(super) relation_only_trove_ids: &'a HashSet<i64>,
    /// Whether surviving installed variants are fixed end-state facts.
    pub(super) lock_surviving_installed: bool,
    /// Pre-existing broken installed groups discharged by identity.
    pub(super) ignored_installed_groups: &'a HashSet<RequirementGroupIdentity>,
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
                ConaryConstraint::FixedIncoming => {}
                ConaryConstraint::ExactInstalledTrove(_) => {}
                ConaryConstraint::ExactSolvables(_) => {}
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
