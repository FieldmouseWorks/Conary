// crates/conary-core/src/resolver/sat/tests/virtual_conditions.rs

#![cfg(test)]

use super::formal_dependencies::insert_repo_pkg_with_reqs;
use super::*;
use crate::db::models::RepositoryProvide;
use crate::repository::dependency_model::RepositoryRequirementKind;
use crate::repository::requirement::parse_native_requirement;

/// Issue #1126: a condition atom is true exactly when a selected solvable
/// satisfies it the same way a positive requirement would, including by
/// provided capability.
#[test]
fn conditions_fire_on_provided_capabilities() {
    let (_dir, conn) = setup_test_db();
    let repository_id = authority_repository(&conn);
    let baz_id = insert_repo_pkg_with_reqs(
        &conn,
        repository_id,
        "baz",
        "1-1",
        "https://example.invalid/baz.rpm",
        "rpm",
        &[],
    );
    provide(&conn, baz_id, "bar");
    insert_repo_pkg_with_reqs(
        &conn,
        repository_id,
        "foo",
        "1-1",
        "https://example.invalid/foo.rpm",
        "rpm",
        &[],
    );

    let cases: &[(&[&str], &[&str])] = &[
        (&["baz"], &["baz"]),
        (&["baz", "(foo if baz)"], &["baz", "foo"]),
        (&["baz", "(foo if bar)"], &["baz", "foo"]),
    ];
    for &(roots, expected) in cases {
        let result = solve(&conn, roots);
        assert!(
            result.conflict_message.is_none(),
            "roots {roots:?}: {result:?}"
        );
        assert_eq!(
            selected_names(&result),
            expected,
            "roots {roots:?}: {result:?}"
        );
    }
}

fn authority_repository(conn: &Connection) -> i64 {
    let mut repository = Repository::new(
        "fedora-44".to_string(),
        "https://example.invalid/fedora".to_string(),
    );
    repository.source_profile = Some("fedora-44".to_string());
    repository.insert(conn).unwrap()
}

fn provide(conn: &Connection, package_id: i64, capability: &str) {
    RepositoryProvide::new(
        package_id,
        capability.to_string(),
        None,
        "virtual".to_string(),
        None,
        VersionScheme::Rpm,
    )
    .insert(conn)
    .unwrap();
}

fn solve(conn: &Connection, roots: &[&str]) -> SatResolution {
    let groups = roots
        .iter()
        .map(|native| {
            parse_native_requirement(
                RepositoryRequirementKind::Depends,
                VersionScheme::Rpm,
                native,
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    solve_requirement_groups_with_outgoing_and_policy(
        conn,
        &groups,
        VersionScheme::Rpm,
        &[],
        &ResolutionPolicy::new().with_primary_source_identity("fedora-44"),
    )
    .unwrap()
}

fn selected_names(result: &SatResolution) -> Vec<&str> {
    let mut names = result
        .install_order
        .iter()
        .map(|package| package.name.as_str())
        .collect::<Vec<_>>();
    names.sort_unstable();
    names
}
