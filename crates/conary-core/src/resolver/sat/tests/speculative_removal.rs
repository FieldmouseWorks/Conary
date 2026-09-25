// crates/conary-core/src/resolver/sat/tests/speculative_removal.rs

#![cfg(test)]

use super::formal_dependencies::insert_repo_pkg_with_reqs;
use super::strict_installed::{hard_depends, repository_fixture};
use super::*;
use crate::db::models::RepositoryProvide;

/// Installed `consumer` depends on installed `oldlib`. `obsoleter` provides
/// `cap` and obsoletes `oldlib`; `keeper`, when present, provides `cap` and
/// removes nothing. Returns the consumer's trove ID.
fn obsoleting_alternative_fixture(conn: &Connection, with_keeper: bool) -> i64 {
    let repository_id = repository_fixture(conn);
    insert_rpm_trove(conn, "oldlib", "1.0.0", &[]);
    let consumer_id = insert_rpm_trove(conn, "consumer", "1.0.0", &[("oldlib", None)]);
    let mut providers = vec![("obsoleter", "9-1")];
    if with_keeper {
        providers.push(("keeper", "1-1"));
    }
    for (name, version) in providers {
        let package_id = insert_repo_pkg_with_reqs(
            conn,
            repository_id,
            name,
            version,
            &format!("https://example.invalid/{name}.rpm"),
            "rpm",
            &[],
        );
        RepositoryProvide::new(
            package_id,
            "cap".to_string(),
            None,
            "virtual".to_string(),
            None,
            VersionScheme::Rpm,
        )
        .insert(conn)
        .unwrap();
        if name == "obsoleter" {
            let obsolete = crate::repository::package_relation::parse_native_relation(
                RepositoryRequirementKind::Obsolete,
                VersionScheme::Rpm,
                "oldlib < 2",
            )
            .unwrap();
            insert_typed_repo_requirement_group(conn, package_id, &obsolete);
        }
    }
    consumer_id
}

fn solve_cap(conn: &Connection) -> SatResolution {
    solve_requirement_groups_with_outgoing_and_policy(
        conn,
        &[hard_depends("cap")],
        VersionScheme::Rpm,
        &[],
        &ResolutionPolicy::new().with_primary_source_identity("fedora-44"),
    )
    .unwrap()
}

#[test]
fn conflict_after_speculative_removal_retries_the_keeping_alternative() {
    let (_dir, conn) = setup_test_db();
    obsoleting_alternative_fixture(&conn, true);

    // Selecting the obsoleter would remove `oldlib` and break `consumer`, so
    // the valid end state installs `keeper` and keeps `oldlib`.
    let resolved = solve_cap(&conn);
    assert!(resolved.conflict_message.is_none(), "{resolved:?}");
    let names = resolved
        .install_order
        .iter()
        .map(|package| package.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["keeper"], "{resolved:?}");
    assert!(resolved.remove_order.is_empty(), "{resolved:?}");
}

#[test]
fn obsoleting_only_provider_is_refused_naming_the_broken_consumer() {
    let (_dir, conn) = setup_test_db();
    let consumer_id = obsoleting_alternative_fixture(&conn, false);

    // Negative control through the same fixture: without `keeper`, every
    // provider of `cap` removes the consumer's dependency.
    let refused = solve_cap(&conn);
    assert!(refused.conflict_message.is_some(), "{refused:?}");
    assert!(refused.install_order.is_empty(), "{refused:?}");
    assert_eq!(
        refused
            .unsatisfied_groups
            .iter()
            .map(|group| group.owner.clone())
            .collect::<Vec<_>>(),
        [SatGroupOwner::Installed {
            trove_id: consumer_id,
            package_name: "consumer".to_string(),
        }],
        "{refused:?}"
    );
}
