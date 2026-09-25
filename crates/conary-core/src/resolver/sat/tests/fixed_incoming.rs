// crates/conary-core/src/resolver/sat/tests/fixed_incoming.rs

#![cfg(test)]

use super::formal_dependencies::insert_repo_pkg_with_reqs;
use super::*;
use crate::packages::traits::PackageFile;
use crate::packages::{PackageFormat, PackagePayload};
use crate::repository::dependency_model::{
    CapabilityProvenance, ProvideArchitectureQualifier, ProvidedCapability,
    RepositoryCapabilityKind, RepositoryRequirementGroup, RepositoryRequirementKind,
};
use crate::repository::requirement::parse_native_requirement;

/// The incoming package under test, constructed directly instead of parsed.
struct TestIncoming {
    name: &'static str,
    version: &'static str,
    requirements: Vec<RepositoryRequirementGroup>,
    capabilities: Vec<ProvidedCapability>,
}

impl PackageFormat for TestIncoming {
    fn parse(_path: &str) -> crate::Result<Self> {
        unreachable!("test package is constructed directly")
    }

    fn name(&self) -> &str {
        self.name
    }

    fn version(&self) -> &str {
        self.version
    }

    fn version_scheme(&self) -> VersionScheme {
        VersionScheme::Rpm
    }

    fn architecture(&self) -> Option<&str> {
        Some("x86_64")
    }

    fn description(&self) -> Option<&str> {
        None
    }

    fn files(&self) -> &[PackageFile] {
        &[]
    }

    fn requirements(&self) -> &[RepositoryRequirementGroup] {
        &self.requirements
    }

    fn resolution_capabilities(&self) -> crate::Result<Vec<ProvidedCapability>> {
        Ok(self.capabilities.clone())
    }

    fn package_payload(&self) -> crate::Result<PackagePayload> {
        unreachable!("dependency solving does not read payload")
    }

    fn to_trove(&self) -> crate::db::models::Trove {
        unreachable!("dependency solving does not construct a trove")
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

fn generic_capability(name: &str) -> ProvidedCapability {
    ProvidedCapability {
        kind: RepositoryCapabilityKind::Generic,
        name: name.to_string(),
        version: None,
        version_relation: None,
        version_scheme: VersionScheme::Rpm,
        architecture_qualifier: ProvideArchitectureQualifier::Implicit,
        provenance: CapabilityProvenance::AuthorDeclared,
    }
}

fn parse_groups(natives: &[&str]) -> Vec<RepositoryRequirementGroup> {
    natives
        .iter()
        .map(|native| {
            parse_native_requirement(
                RepositoryRequirementKind::Depends,
                VersionScheme::Rpm,
                native,
            )
            .unwrap()
        })
        .collect()
}

fn selected_names(result: &SatResolution) -> Vec<&str> {
    result
        .install_order
        .iter()
        .map(|package| package.name.as_str())
        .collect()
}

#[test]
fn incoming_conditional_conjunction_sees_its_own_provide() {
    let (_dir, conn) = setup_test_db();
    let repository_id = authority_repository(&conn);
    for name in ["baz", "foo"] {
        insert_repo_pkg_with_reqs(
            &conn,
            repository_id,
            name,
            "1-1",
            &format!("https://example.invalid/{name}.rpm"),
            "rpm",
            &[],
        );
    }
    let package = TestIncoming {
        name: "phase4-corpus",
        version: "1.0.0-1",
        requirements: parse_groups(&["baz", "((foo and selfcap) if baz)"]),
        capabilities: vec![generic_capability("selfcap")],
    };
    let policy = ResolutionPolicy::new().with_primary_source_identity("fedora-44");
    let solve = || {
        solve_package_requirements_with_provides_outgoing_and_policy(
            &conn,
            &package,
            package.resolution_capabilities().unwrap(),
            &[],
            &policy,
        )
        .unwrap()
    };

    // The incoming package provides `selfcap`, so selecting `baz` must make the
    // activated conjunction solvable with `foo` from the repository.
    let resolved = solve();
    assert!(resolved.conflict_message.is_none(), "{resolved:?}");
    let mut names = selected_names(&resolved);
    names.sort_unstable();
    assert_eq!(names, ["baz", "foo"], "{resolved:?}");

    // Control through the same fixture: removing the only `foo` provider makes
    // the activated conjunction unsatisfiable, and the refusal is a typed SAT
    // conflict rather than a fabricated install order.
    conn.execute("DELETE FROM repository_packages WHERE name = 'foo'", [])
        .unwrap();
    let refused = solve();
    assert!(refused.conflict_message.is_some(), "{refused:?}");
    assert!(refused.install_order.is_empty(), "{refused:?}");
}

#[test]
fn relation_removed_condition_is_not_reloaded_by_a_later_pass() {
    let (_dir, conn) = setup_test_db();
    let repository_id = authority_repository(&conn);
    let bar_trove_id = insert_rpm_trove(&conn, "bar", "1.0.0", &[]);
    let baz_id = insert_repo_pkg_with_reqs(
        &conn,
        repository_id,
        "baz",
        "2.0-1",
        "https://example.invalid/baz.rpm",
        "rpm",
        &[],
    );
    insert_repo_pkg_with_reqs(
        &conn,
        repository_id,
        "foo",
        "1-1",
        "https://example.invalid/foo.rpm",
        "rpm",
        &[],
    );
    let obsolete = crate::repository::package_relation::parse_native_relation(
        RepositoryRequirementKind::Obsolete,
        VersionScheme::Rpm,
        "bar < 2",
    )
    .unwrap();
    insert_typed_repo_requirement_group(&conn, baz_id, &obsolete);

    let groups = parse_groups(&["baz", "(foo or bar)"]);
    let policy = ResolutionPolicy::new().with_primary_source_identity("fedora-44");
    let solve = || {
        solve_requirement_groups_with_outgoing_and_policy(
            &conn,
            &groups,
            VersionScheme::Rpm,
            &[],
            &policy,
        )
        .unwrap()
    };

    // RPM admits `unless` only in conflict-like tag contexts, so the legal
    // spelling of `foo unless bar` under Depends is the equivalent `foo or
    // bar`. Pass one selects `baz`, whose relation plan removes the installed
    // `bar`, so pass two must exclude `bar` from candidate discovery: the
    // removed branch can only hold with repository `foo`, and the removal stays
    // named in the resolution.
    let resolved = solve();
    assert!(resolved.conflict_message.is_none(), "{resolved:?}");
    let mut names = selected_names(&resolved);
    names.sort_unstable();
    assert_eq!(names, ["baz", "foo"], "{resolved:?}");
    assert_eq!(resolved.remove_order.len(), 1, "{resolved:?}");
    assert_eq!(
        resolved.remove_order[0].trove_id, bar_trove_id,
        "{resolved:?}"
    );

    // Control through the same fixture: without repository `foo` the
    // transaction cannot replace the removed condition, so the solve refuses.
    conn.execute("DELETE FROM repository_packages WHERE name = 'foo'", [])
        .unwrap();
    let refused = solve();
    assert!(refused.conflict_message.is_some(), "{refused:?}");
    assert!(refused.install_order.is_empty(), "{refused:?}");
}

#[test]
fn incoming_name_and_provide_are_satisfied_by_the_fixed_fact() {
    let (_dir, conn) = setup_test_db();
    let repository_id = authority_repository(&conn);
    // A repository candidate carries the incoming's name with a higher version,
    // so ordinary candidate ordering would prefer it without the fixed fact.
    insert_repo_pkg_with_reqs(
        &conn,
        repository_id,
        "selfhosted",
        "3-1",
        "https://example.invalid/selfhosted.rpm",
        "rpm",
        &[],
    );
    insert_repo_pkg_with_reqs(
        &conn,
        repository_id,
        "external",
        "1-1",
        "https://example.invalid/external.rpm",
        "rpm",
        &[],
    );
    let package = TestIncoming {
        name: "selfhosted",
        version: "2.0-1",
        requirements: parse_groups(&["(external and selfhosted >= 2.0-1 and selfcap)"]),
        capabilities: vec![generic_capability("selfcap")],
    };
    let policy = ResolutionPolicy::new().with_primary_source_identity("fedora-44");

    let resolved = solve_package_requirements_with_provides_outgoing_and_policy(
        &conn,
        &package,
        package.resolution_capabilities().unwrap(),
        &[],
        &policy,
    )
    .unwrap();

    assert!(resolved.conflict_message.is_none(), "{resolved:?}");
    // Both the name requirement and the capability requirement are satisfied by
    // the fixed incoming solvable; the same-name repository candidate is not
    // installed and the fixed fact never enters the install order.
    assert_eq!(selected_names(&resolved), ["external"], "{resolved:?}");
}
