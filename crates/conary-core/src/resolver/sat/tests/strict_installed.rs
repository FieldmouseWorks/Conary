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
fn installed_provider_satisfies_strict_requirement_without_repository_authority() {
    let (_dir, conn) = setup_test_db();
    let repository_id = repository_fixture(&conn);
    installed_file_provider(&conn, "installed-provider");
    repository_file_provider(&conn, repository_id, "repository-provider");

    let result = solve_requirement_groups_with_policy(
        &conn,
        &[file_predepends()],
        VersionScheme::Rpm,
        &strict_policy_without_source_authority(&conn),
    )
    .unwrap();

    assert!(result.install_order.is_empty(), "{result:?}");
    assert!(result.remove_order.is_empty(), "{result:?}");
    assert_eq!(result.conflict_message, None);
}

#[test]
fn strict_requirement_without_installed_provider_is_refused() {
    let (_dir, conn) = setup_test_db();
    let repository_id = repository_fixture(&conn);
    repository_file_provider(&conn, repository_id, "repository-provider");

    let error = solve_requirement_groups_with_policy(
        &conn,
        &[file_predepends()],
        VersionScheme::Rpm,
        &strict_policy_without_source_authority(&conn),
    )
    .unwrap_err();

    assert!(matches!(error, Error::ConfigError(_)), "{error:?}");
}

#[test]
fn malformed_source_identity_is_refused_before_installed_fallback() {
    let (_dir, conn) = setup_test_db();
    installed_file_provider(&conn, "installed-provider");

    // The installed set could satisfy the requirement, but a malformed
    // identity is invalid authority rather than absent authority.
    let policy = ResolutionPolicy::new().with_primary_source_identity(" bad identity ");
    let error = solve_requirement_groups_with_policy(
        &conn,
        &[file_predepends()],
        VersionScheme::Rpm,
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
