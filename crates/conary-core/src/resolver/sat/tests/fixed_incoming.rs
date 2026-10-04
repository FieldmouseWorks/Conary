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
fn incoming_provide_activates_conditional_requirement_against_repository() {
    let (_dir, conn) = setup_test_db();
    let repository_id = authority_repository(&conn);
    insert_repo_pkg_with_reqs(
        &conn,
        repository_id,
        "foo",
        "1-1",
        "https://example.invalid/foo.rpm",
        "rpm",
        &[],
    );
    // The incoming package provides `bar` and declares `foo if bar`, so its own
    // fixed fact makes the condition true and `foo` must be installed.
    let package = TestIncoming {
        name: "conditional-provider",
        version: "1.0.0-1",
        requirements: parse_groups(&["(foo if bar)"]),
        capabilities: vec![generic_capability("bar")],
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

    let resolved = solve();
    assert!(resolved.conflict_message.is_none(), "{resolved:?}");
    assert_eq!(selected_names(&resolved), ["foo"], "{resolved:?}");

    // Control through the same fixture: removing the only `foo` provider makes
    // the activated requirement unsatisfiable, and the refusal is a typed SAT
    // conflict rather than an install order that omits the dependency.
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
fn installed_root_does_not_load_an_unreferenced_obsoleter() {
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
        "helper",
        "1-1",
        "https://example.invalid/helper.rpm",
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

    // `helper` keeps the transaction on the SAT path: the positive `bar` root
    // alone is already held by the fixed end state. Nothing names `baz`, so its
    // typed relation never loads and the forced `bar` root has no remover
    // alternative. The disjunction must not pull the obsoleter into the solve.
    let groups = parse_groups(&["bar", "helper"]);
    let policy = ResolutionPolicy::new().with_primary_source_identity("fedora-44");
    let resolved = solve_requirement_groups_with_outgoing_and_policy(
        &conn,
        &groups,
        VersionScheme::Rpm,
        &[],
        &policy,
    )
    .unwrap();

    assert!(resolved.conflict_message.is_none(), "{resolved:?}");
    assert_eq!(selected_names(&resolved), ["helper"], "{resolved:?}");
    assert!(
        resolved
            .install_order
            .iter()
            .all(|package| package.name != "baz"),
        "{resolved:?}"
    );
    assert!(resolved.remove_order.is_empty(), "{resolved:?}");
    assert!(
        resolved
            .remove_order
            .iter()
            .all(|removal| removal.trove_id != bar_trove_id),
        "{resolved:?}"
    );
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

/// Insert an installed RPM package trove with an explicit architecture, so a
/// parallel install of the incoming package's own name can be represented.
fn insert_installed_trove_for_architecture(
    conn: &Connection,
    name: &str,
    version: &str,
    architecture: &str,
) -> i64 {
    let trove_id = insert_rpm_trove(conn, name, version, &[]);
    conn.execute(
        "UPDATE troves SET architecture = ?1 WHERE id = ?2",
        rusqlite::params![architecture, trove_id],
    )
    .unwrap();
    trove_id
}

#[test]
fn incoming_coexists_with_surviving_same_name_variant() {
    let (_dir, conn) = setup_test_db();
    let repository_id = authority_repository(&conn);
    // `bar` is only in the repository, so the solve must reach SAT even though
    // the incoming's own name is already held by a surviving variant.
    insert_repo_pkg_with_reqs(
        &conn,
        repository_id,
        "bar",
        "1-1",
        "https://example.invalid/bar.rpm",
        "rpm",
        &[],
    );
    let surviving_variant =
        insert_installed_trove_for_architecture(&conn, "libfoo", "1.0.0", "aarch64");
    let policy = ResolutionPolicy::new().with_primary_source_identity("fedora-44");

    let package = TestIncoming {
        name: "libfoo",
        version: "2.0-1",
        requirements: parse_groups(&["bar"]),
        capabilities: Vec::new(),
    };
    let resolved = solve_package_requirements_with_provides_outgoing_and_policy(
        &conn,
        &package,
        package.resolution_capabilities().unwrap(),
        &[],
        &policy,
    )
    .unwrap();

    // The incoming `x86_64` package and the surviving `aarch64` variant are both
    // facts of the end state, so SAT must select both while installing only the
    // repository `bar`.
    assert!(resolved.conflict_message.is_none(), "{resolved:?}");
    assert_eq!(selected_names(&resolved), ["bar"], "{resolved:?}");
    assert!(
        resolved
            .install_order
            .iter()
            .all(|package| package.installed_trove_id != Some(surviving_variant)),
        "{resolved:?}"
    );

    // Control through the same fixture: the incoming requires a third
    // repository version of its own name. That repository candidate would
    // satisfy the requirement if it were allowed, but it must not replace the
    // surviving variant, so the requirement is refused typed.
    insert_repo_pkg_with_reqs(
        &conn,
        repository_id,
        "libfoo",
        "3.0.0",
        "https://example.invalid/libfoo.rpm",
        "rpm",
        &[],
    );
    let replacement = TestIncoming {
        name: "libfoo",
        version: "2.0-1",
        requirements: parse_groups(&["libfoo = 3.0.0"]),
        capabilities: Vec::new(),
    };
    let refused = solve_package_requirements_with_provides_outgoing_and_policy(
        &conn,
        &replacement,
        replacement.resolution_capabilities().unwrap(),
        &[],
        &policy,
    )
    .unwrap();
    assert!(refused.conflict_message.is_some(), "{refused:?}");
    assert!(refused.install_order.is_empty(), "{refused:?}");
}
