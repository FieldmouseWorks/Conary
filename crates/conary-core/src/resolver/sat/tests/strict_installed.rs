// crates/conary-core/src/resolver/sat/tests/strict_installed.rs

#![cfg(test)]

use super::*;
use crate::db::models::{ProvideEntry, RepositoryProvide};
use crate::repository::dependency_model::{
    RepositoryCapabilityKind, RepositoryRequirementClause, RepositoryRequirementGroup,
    RepositoryRequirementKind,
};
use crate::repository::load_effective_policy;
use crate::repository::resolution_policy::RequestScope;

const PROVIDER_PATH: &str = "/bin/sh";

fn installed_file_provider(conn: &Connection, name: &str) -> i64 {
    let trove_id = insert_rpm_trove(conn, name, "1.0.0", &[]);
    let mut provide = ProvideEntry::new_typed(
        trove_id,
        RepositoryCapabilityKind::File,
        PROVIDER_PATH.to_string(),
        None,
        VersionScheme::Rpm,
        Default::default(),
    );
    provide.insert(conn).unwrap();
    trove_id
}

fn repository_file_provider(conn: &Connection, repository_id: i64, name: &str) -> i64 {
    let package_id = insert_rpm_repo_package(conn, repository_id, name, "1-1");
    let mut provide = RepositoryProvide::new(
        package_id,
        PROVIDER_PATH.to_string(),
        None,
        "file".to_string(),
        None,
        VersionScheme::Rpm,
    );
    provide.insert(conn).unwrap();
    package_id
}

fn file_predepends() -> RepositoryRequirementGroup {
    let mut clause = RepositoryRequirementClause::name_only(PROVIDER_PATH.to_string());
    clause.capability_kind = Some(RepositoryCapabilityKind::File);
    RepositoryRequirementGroup::simple(RepositoryRequirementKind::PreDepends, clause)
}

fn conditional_depends(required: &str, condition: &str) -> RepositoryRequirementGroup {
    crate::repository::requirement::parse_native_requirement(
        RepositoryRequirementKind::Depends,
        VersionScheme::Rpm,
        &format!("({required} if {condition})"),
    )
    .unwrap()
}

fn strict_policy_without_source_authority(conn: &Connection) -> ResolutionPolicy {
    load_effective_policy(conn, RequestScope::Any)
        .unwrap()
        .resolution
}

fn repository_fixture(conn: &Connection) -> i64 {
    let mut repository = Repository::new(
        "fedora-44".to_string(),
        "https://example.invalid/fedora".to_string(),
    );
    repository.source_profile = Some("fedora-44".to_string());
    repository.insert(conn).unwrap()
}

#[test]
fn unknown_end_state_under_strict_policy_is_refused() {
    let (_dir, conn) = setup_test_db();
    let repository_id = repository_fixture(&conn);
    installed_file_provider(&conn, "installed-provider");
    repository_file_provider(&conn, repository_id, "repository-provider");
    let policy = strict_policy_without_source_authority(&conn);

    // The caller did not say which installed troves the transaction removes, so
    // the installed provider cannot be trusted to survive it.
    let error = solve_requirement_groups_with_policy(
        &conn,
        &[file_predepends()],
        VersionScheme::Rpm,
        &policy,
    )
    .unwrap_err();
    assert!(matches!(error, Error::ConfigError(_)), "{error:?}");

    // A known empty outgoing set makes the same input satisfiable.
    let satisfied = solve_requirement_groups_with_outgoing_and_policy(
        &conn,
        &[file_predepends()],
        VersionScheme::Rpm,
        &[],
        &policy,
    )
    .unwrap();
    assert!(satisfied.install_order.is_empty(), "{satisfied:?}");
    assert!(satisfied.remove_order.is_empty(), "{satisfied:?}");
    assert_eq!(satisfied.conflict_message, None);
}

#[test]
fn strict_requirement_without_installed_provider_is_refused() {
    let (_dir, conn) = setup_test_db();
    let repository_id = repository_fixture(&conn);
    repository_file_provider(&conn, repository_id, "repository-provider");

    // Negative control for the installed-provider case: the known end state is
    // empty, so the requirement has no provider and must be refused.
    let error = solve_requirement_groups_with_outgoing_and_policy(
        &conn,
        &[file_predepends()],
        VersionScheme::Rpm,
        &[],
        &strict_policy_without_source_authority(&conn),
    )
    .unwrap_err();

    assert!(matches!(error, Error::ConfigError(_)), "{error:?}");
}

#[test]
fn conditional_requirement_is_refused_when_installed_condition_is_true() {
    let (_dir, conn) = setup_test_db();
    let policy = strict_policy_without_source_authority(&conn);

    // Control: the condition is absent, so the implication is genuinely vacuous.
    let vacuous = solve_requirement_groups_with_outgoing_and_policy(
        &conn,
        &[conditional_depends("foo", "bar")],
        VersionScheme::Rpm,
        &[],
        &policy,
    )
    .unwrap();
    assert_eq!(vacuous.conflict_message, None, "{vacuous:?}");

    // `bar` is installed and survives the transaction, so `foo` must be present
    // even though the SAT solver could otherwise leave `bar` out.
    insert_rpm_trove(&conn, "bar", "1.0.0", &[]);
    let error = solve_requirement_groups_with_outgoing_and_policy(
        &conn,
        &[conditional_depends("foo", "bar")],
        VersionScheme::Rpm,
        &[],
        &policy,
    )
    .unwrap_err();
    assert!(matches!(error, Error::ConfigError(_)), "{error:?}");
}

#[test]
fn conditional_requirement_is_satisfied_when_both_sides_are_installed() {
    let (_dir, conn) = setup_test_db();
    insert_rpm_trove(&conn, "bar", "1.0.0", &[]);
    insert_rpm_trove(&conn, "foo", "1.0.0", &[]);

    let satisfied = solve_requirement_groups_with_outgoing_and_policy(
        &conn,
        &[conditional_depends("foo", "bar")],
        VersionScheme::Rpm,
        &[],
        &strict_policy_without_source_authority(&conn),
    )
    .unwrap();
    assert!(satisfied.install_order.is_empty(), "{satisfied:?}");
    assert!(satisfied.remove_order.is_empty(), "{satisfied:?}");
    assert_eq!(satisfied.conflict_message, None);
}

#[test]
fn outgoing_installed_provider_is_excluded_from_strict_installed_solve() {
    let (_dir, conn) = setup_test_db();
    let provider_trove_id = installed_file_provider(&conn, "installed-provider");
    let policy = strict_policy_without_source_authority(&conn);

    // Positive control: while the provider is not outgoing, the installed-only
    // path satisfies the requirement with an empty install order.
    let satisfied = solve_requirement_groups_with_outgoing_and_policy(
        &conn,
        &[file_predepends()],
        VersionScheme::Rpm,
        &[],
        &policy,
    )
    .unwrap();
    assert!(satisfied.install_order.is_empty(), "{satisfied:?}");
    assert!(satisfied.remove_order.is_empty(), "{satisfied:?}");
    assert_eq!(satisfied.conflict_message, None);

    // With the provider outgoing, the end state has no provider, so the
    // installed-only path must refuse rather than use a package the
    // transaction removes.
    let error = solve_requirement_groups_with_outgoing_and_policy(
        &conn,
        &[file_predepends()],
        VersionScheme::Rpm,
        &[provider_trove_id],
        &policy,
    )
    .unwrap_err();
    assert!(matches!(error, Error::ConfigError(_)), "{error:?}");
}

#[test]
fn repository_authority_does_not_use_outgoing_installed_provider() {
    let (_dir, conn) = setup_test_db();
    let repository_id = repository_fixture(&conn);
    let provider_trove_id = installed_file_provider(&conn, "installed-provider");
    repository_file_provider(&conn, repository_id, "repository-provider");
    let policy = ResolutionPolicy::new().with_primary_source_identity("fedora-44");

    // Positive control: with a repository candidate admitted and the installed
    // provider not outgoing, the authority path solves.
    let surviving = solve_requirement_groups_with_outgoing_and_policy(
        &conn,
        &[file_predepends()],
        VersionScheme::Rpm,
        &[],
        &policy,
    )
    .unwrap();
    assert!(surviving.conflict_message.is_none(), "{surviving:?}");

    // With the installed provider outgoing, only the repository candidate can
    // satisfy the requirement.
    let replaced = solve_requirement_groups_with_outgoing_and_policy(
        &conn,
        &[file_predepends()],
        VersionScheme::Rpm,
        &[provider_trove_id],
        &policy,
    )
    .unwrap();
    assert!(replaced.conflict_message.is_none(), "{replaced:?}");
    assert!(
        replaced.install_order.iter().any(|package| {
            package.name == "repository-provider" && package.source == SatSource::Repository
        }),
        "{replaced:?}"
    );
    assert!(
        replaced
            .install_order
            .iter()
            .all(|package| package.installed_trove_id != Some(provider_trove_id)),
        "{replaced:?}"
    );
}

#[test]
fn malformed_source_identity_is_refused_before_installed_fallback() {
    let (_dir, conn) = setup_test_db();
    installed_file_provider(&conn, "installed-provider");

    // The installed set could satisfy the requirement, but a malformed
    // identity is invalid authority rather than absent authority. The known
    // empty end state would otherwise succeed, so the refusal must come from
    // identity validation.
    let policy = ResolutionPolicy::new().with_primary_source_identity(" bad identity ");
    let error = solve_requirement_groups_with_outgoing_and_policy(
        &conn,
        &[file_predepends()],
        VersionScheme::Rpm,
        &[],
        &policy,
    )
    .unwrap_err();

    assert!(matches!(error, Error::ConfigError(_)), "{error:?}");
}

#[test]
fn repository_authority_still_admits_repository_candidate() {
    let (_dir, conn) = setup_test_db();
    let repository_id = repository_fixture(&conn);
    repository_file_provider(&conn, repository_id, "repository-provider");

    let policy = ResolutionPolicy::new().with_primary_source_identity("fedora-44");
    let result = solve_requirement_groups_with_policy(
        &conn,
        &[file_predepends()],
        VersionScheme::Rpm,
        &policy,
    )
    .unwrap();

    assert!(result.conflict_message.is_none(), "{result:?}");
    assert!(
        result
            .install_order
            .iter()
            .any(|package| package.name == "repository-provider"),
        "{result:?}"
    );
}
