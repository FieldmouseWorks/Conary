// crates/conary-core/src/resolver/sat/tests/slot_replacement.rs

#![cfg(test)]

use super::strict_installed::{hard_depends, repository_fixture};
use super::*;
use crate::db::models::{RepositoryProvide, Trove, TroveType};

fn authority_policy() -> ResolutionPolicy {
    ResolutionPolicy::new().with_primary_source_identity("fedora-44")
}

fn solve(
    conn: &Connection,
    groups: &[crate::repository::dependency_model::RepositoryRequirementGroup],
) -> SatResolution {
    solve_requirement_groups_with_outgoing_and_policy(
        conn,
        groups,
        VersionScheme::Rpm,
        &[],
        &authority_policy(),
    )
    .unwrap()
}

fn installed_versions(result: &SatResolution) -> Vec<(&str, &str)> {
    result
        .install_order
        .iter()
        .map(|package| (package.name.as_str(), package.version.as_str()))
        .collect()
}

#[test]
fn authority_known_end_state_upgrades_a_surviving_installed_dependency() {
    let (_dir, conn) = setup_test_db();
    let repository_id = repository_fixture(&conn);
    let foo_id = insert_rpm_trove(&conn, "foo", "1.0.0", &[]);
    insert_rpm_repo_package(&conn, repository_id, "foo", "3-1");
    let requirement = hard_depends("foo >= 2.0.0");

    // The surviving `foo` 1.0.0 cannot satisfy the requirement. The repository
    // `foo` 3-1 shares its install slot, so the solve plans the upgrade the
    // installer makes instead of refusing.
    let upgraded = solve(&conn, std::slice::from_ref(&requirement));
    assert!(upgraded.conflict_message.is_none(), "{upgraded:?}");
    assert_eq!(installed_versions(&upgraded), [("foo", "3-1")]);
    assert_eq!(upgraded.install_order[0].source, SatSource::Repository);

    // Negative through the same fixture: a pinned trove has no replacer, so
    // the same requirement is refused.
    Trove::pin(&conn, foo_id).unwrap();
    let refused = solve(&conn, &[requirement]);
    assert!(refused.conflict_message.is_some(), "{refused:?}");
    assert!(refused.install_order.is_empty(), "{refused:?}");
}

#[test]
fn upgrade_refusal_names_the_installed_dependent_of_the_replaced_version() {
    let (_dir, conn) = setup_test_db();
    let repository_id = repository_fixture(&conn);
    let foo_id = insert_rpm_trove(&conn, "foo", "1.0.0", &[]);
    insert_provide(&conn, foo_id, "foo-abi-1", None);
    let consumer_id = insert_rpm_trove(&conn, "consumer", "1.0.0", &[("foo-abi-1", None)]);
    let upgrade_id = insert_rpm_repo_package(&conn, repository_id, "foo", "3-1");
    let requirement = hard_depends("foo >= 2.0.0");

    // The upgrade replaces the only provider of the consumer's capability, so
    // the end state would break the surviving consumer and the refusal must
    // name it.
    let refused = solve(&conn, std::slice::from_ref(&requirement));
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

    // Positive control through the same fixture: once the upgrade keeps the
    // capability, the consumer holds and the upgrade is planned.
    RepositoryProvide::new(
        upgrade_id,
        "foo-abi-1".to_string(),
        None,
        "virtual".to_string(),
        None,
        VersionScheme::Rpm,
    )
    .insert(&conn)
    .unwrap();
    let upgraded = solve(&conn, &[requirement]);
    assert!(upgraded.conflict_message.is_none(), "{upgraded:?}");
    assert_eq!(installed_versions(&upgraded), [("foo", "3-1")]);
}

#[test]
fn upgrade_replaces_one_install_slot_and_keeps_the_parallel_variant() {
    let (_dir, conn) = setup_test_db();
    let repository_id = repository_fixture(&conn);
    insert_rpm_trove(&conn, "foo", "1.0.0", &[]);
    let mut foreign = Trove::new(
        "foo".to_string(),
        "1.0.0".to_string(),
        TroveType::Package,
        VersionScheme::Rpm,
    );
    foreign.architecture = Some("i686".to_string());
    let foreign_id = foreign.insert(&conn).unwrap();
    insert_provide(&conn, foreign_id, "foo-i686-cap", None);
    insert_rpm_repo_package(&conn, repository_id, "foo", "3-1");

    // The x86_64 variant is the upgrade's only slot predecessor; the i686
    // variant survives beside it, so a requirement on its capability still
    // holds in the same solve.
    let upgraded = solve(
        &conn,
        &[hard_depends("foo >= 2.0.0"), hard_depends("foo-i686-cap")],
    );
    assert!(upgraded.conflict_message.is_none(), "{upgraded:?}");
    assert_eq!(installed_versions(&upgraded), [("foo", "3-1")]);
}
