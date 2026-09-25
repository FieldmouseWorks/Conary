// crates/conary-core/src/resolver/sat/tests/root_requirements.rs

#![cfg(test)]

use super::formal_dependencies::insert_repo_pkg_with_reqs;
use super::*;
use crate::db::models::RepositoryProvide;
use crate::packages::traits::PackageFile;
use crate::packages::{PackageFormat, PackagePayload};
use crate::repository::dependency_model::{
    CapabilityProvenance, ProvideArchitectureQualifier, ProvideVersionRelation, ProvidedCapability,
    RepositoryCapabilityKind, RepositoryRequirementClause, RepositoryRequirementGroup,
    RepositoryRequirementKind, SourcePackageFormat,
};
use crate::repository::requirement::parse_native_requirement;

struct IncomingPackage {
    requirements: Vec<crate::repository::dependency_model::RepositoryRequirementGroup>,
    capabilities: Vec<ProvidedCapability>,
}

impl PackageFormat for IncomingPackage {
    fn parse(_path: &str) -> crate::Result<Self> {
        unreachable!("test package is constructed directly")
    }

    fn name(&self) -> &str {
        "phase4-corpus"
    }

    fn version(&self) -> &str {
        "1.0.0-1"
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

    fn requirements(&self) -> &[crate::repository::dependency_model::RepositoryRequirementGroup] {
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

fn repository(conn: &Connection) -> i64 {
    let mut repository = Repository::new(
        "rich-root".to_string(),
        "https://rich-root.invalid".to_string(),
    );
    repository.insert(conn).unwrap()
}

fn authority_repository(conn: &Connection) -> i64 {
    let mut repository = Repository::new(
        "rich-root".to_string(),
        "https://rich-root.invalid".to_string(),
    );
    repository.source_profile = Some("fedora-44".to_string());
    repository.insert(conn).unwrap()
}

fn package(conn: &Connection, repository_id: i64, name: &str, provides: &[&str]) {
    let package_id = insert_repo_pkg_with_reqs(
        conn,
        repository_id,
        name,
        "1-1",
        &format!("https://rich-root.invalid/{name}.rpm"),
        "rpm",
        &[],
    );
    for capability in provides {
        RepositoryProvide::new(
            package_id,
            (*capability).to_string(),
            None,
            "virtual".to_string(),
            None,
            VersionScheme::Rpm,
        )
        .insert(conn)
        .unwrap();
    }
}

fn solve_rpm_root(conn: &Connection, native: &str) -> SatResolution {
    solve_rpm_roots(conn, &[native])
}

fn solve_rpm_roots(conn: &Connection, native: &[&str]) -> SatResolution {
    let groups = native
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
    solve_requirement_groups_with_policy(
        conn,
        &groups,
        VersionScheme::Rpm,
        &ResolutionPolicy::new()
            .with_mixing(crate::repository::resolution_policy::DependencyMixingPolicy::Permissive),
    )
    .unwrap()
}

fn selected_names(result: &SatResolution) -> Vec<&str> {
    result
        .install_order
        .iter()
        .map(|package| package.name.as_str())
        .collect()
}

#[test]
fn incoming_exact_provide_satisfies_only_its_matching_positive_requirement() {
    let self_requirement = parse_native_requirement(
        RepositoryRequirementKind::Depends,
        VersionScheme::Rpm,
        "config(phase4-corpus) = 1.0.0-1",
    )
    .unwrap();
    let external_requirement = parse_native_requirement(
        RepositoryRequirementKind::Depends,
        VersionScheme::Rpm,
        "phase4-repository-fixture = 1.0.0",
    )
    .unwrap();
    let incoming = PackageIdentity {
        repo_package_id: None,
        name: "phase4-corpus".to_string(),
        version: "1.0.0-1".to_string(),
        package_release: None,
        architecture: Some("x86_64".to_string()),
        debian_multi_arch: None,
        version_scheme: VersionScheme::Rpm,
        repository_id: None,
        repository_name: String::new(),
        repository_profile: None,
        repository_priority: 0,
        canonical_id: None,
        canonical_name: None,
        installed_trove_id: None,
        installed_pinned: false,
        provided_capabilities: vec![ProvidedCapability {
            kind: RepositoryCapabilityKind::Generic,
            name: "config(phase4-corpus)".to_string(),
            version: Some("1.0.0-1".to_string()),
            version_relation: Some(ProvideVersionRelation::Equal),
            version_scheme: VersionScheme::Rpm,
            architecture_qualifier: ProvideArchitectureQualifier::Implicit,
            provenance: CapabilityProvenance::SourceDeclared {
                format: SourcePackageFormat::Rpm,
                record_index: 0,
            },
        }],
    };

    assert!(
        positive_requirement_group_satisfied_by_package(
            &self_requirement,
            VersionScheme::Rpm,
            &incoming,
        )
        .unwrap()
    );
    assert!(
        !positive_requirement_group_satisfied_by_package(
            &external_requirement,
            VersionScheme::Rpm,
            &incoming,
        )
        .unwrap()
    );
}

#[test]
fn package_solver_discharge_self_provide_but_selects_external_dependency() {
    let (_temp, conn) = setup_test_db();
    let repository_id = repository(&conn);
    insert_repo_pkg_with_reqs(
        &conn,
        repository_id,
        "phase4-repository-fixture",
        "1.0.0",
        "https://rich-root.invalid/phase4-repository-fixture.rpm",
        "rpm",
        &[],
    );
    let requirements = [
        "config(phase4-corpus) = 1.0.0-1",
        "phase4-repository-fixture = 1.0.0",
    ]
    .into_iter()
    .map(|native| {
        parse_native_requirement(
            RepositoryRequirementKind::Depends,
            VersionScheme::Rpm,
            native,
        )
        .unwrap()
    })
    .collect();
    let package = IncomingPackage {
        requirements,
        capabilities: vec![ProvidedCapability {
            kind: RepositoryCapabilityKind::Generic,
            name: "config(phase4-corpus)".to_string(),
            version: Some("1.0.0-1".to_string()),
            version_relation: Some(ProvideVersionRelation::Equal),
            version_scheme: VersionScheme::Rpm,
            architecture_qualifier: ProvideArchitectureQualifier::Implicit,
            provenance: CapabilityProvenance::SourceDeclared {
                format: SourcePackageFormat::Rpm,
                record_index: 0,
            },
        }],
    };

    let result = solve_package_requirements_with_policy(
        &conn,
        &package,
        &ResolutionPolicy::new()
            .with_mixing(crate::repository::resolution_policy::DependencyMixingPolicy::Permissive),
    )
    .unwrap();
    assert!(result.conflict_message.is_none(), "{result:?}");
    assert_eq!(selected_names(&result), ["phase4-repository-fixture"]);
}

#[test]
fn root_and_or_and_version_constraints_reach_sat_exactly() {
    let (_temp, conn) = setup_test_db();
    let repository_id = repository(&conn);
    package(&conn, repository_id, "alpha", &[]);
    package(&conn, repository_id, "beta", &[]);
    package(&conn, repository_id, "gamma", &[]);

    let result = solve_rpm_root(&conn, "(alpha >= 1 and (beta or gamma))");
    assert!(result.conflict_message.is_none(), "{result:?}");
    let names = selected_names(&result);
    assert!(names.contains(&"alpha"));
    assert!(names.contains(&"beta") || names.contains(&"gamma"));
}

#[test]
fn root_if_else_preserves_boolean_semantics() {
    let (_temp, conn) = setup_test_db();
    let repository_id = repository(&conn);
    for name in ["condition", "required", "alternate"] {
        package(&conn, repository_id, name, &[]);
    }

    let if_result = solve_rpm_roots(&conn, &["(required if condition)", "condition"]);
    assert!(
        if_result.conflict_message.is_none(),
        "{:?}",
        if_result.conflict_message
    );
    let if_names = selected_names(&if_result);
    assert!(if_names.contains(&"condition"));
    assert!(if_names.contains(&"required"));
}

#[test]
fn root_if_missing_required_returns_conflict_without_panic() {
    let (_temp, conn) = setup_test_db();
    let repository_id = repository(&conn);
    package(&conn, repository_id, "condition", &[]);

    let result = solve_rpm_roots(&conn, &["(required if condition)", "condition"]);
    let conflict = result
        .conflict_message
        .as_ref()
        .expect("triggered conditional with no required provider must be unsatisfiable");

    assert!(!conflict.trim().is_empty());
    assert!(result.install_order.is_empty(), "{result:?}");
}

#[test]
fn root_with_and_without_require_same_provider_facts() {
    let (_temp, conn) = setup_test_db();
    let repository_id = repository(&conn);
    package(&conn, repository_id, "split-a", &["feature-a"]);
    package(&conn, repository_id, "split-b", &["feature-b"]);
    package(
        &conn,
        repository_id,
        "combined",
        &["feature-a", "feature-b"],
    );
    package(&conn, repository_id, "left-only", &["feature-a"]);

    let with_result = solve_rpm_root(&conn, "(feature-a with feature-b)");
    assert!(
        with_result.conflict_message.is_none(),
        "{:?}",
        with_result.conflict_message
    );
    let with_names = selected_names(&with_result);
    assert!(with_names.contains(&"combined"));
    assert!(!(with_names.contains(&"split-a") && with_names.contains(&"split-b")));

    let without_result = solve_rpm_root(&conn, "(feature-a without feature-b)");
    assert!(
        without_result.conflict_message.is_none(),
        "{:?}",
        without_result.conflict_message
    );
    let without_names = selected_names(&without_result);
    assert!(without_names.contains(&"left-only") || without_names.contains(&"split-a"));
    assert!(!without_names.contains(&"combined"));
}

#[test]
fn package_solver_provides_view_governs_file_pre_depends_self_satisfaction() {
    let (_temp, conn) = setup_test_db();
    let requirement = RepositoryRequirementGroup::simple(
        RepositoryRequirementKind::PreDepends,
        RepositoryRequirementClause {
            name: "/bin/sh".to_string(),
            capability_kind: Some(RepositoryCapabilityKind::File),
            version_constraint: None,
            architecture_qualifier: Default::default(),
            native_text: Some("/bin/sh".to_string()),
        },
    );
    let file_provide = ProvidedCapability {
        kind: RepositoryCapabilityKind::File,
        name: "/bin/sh".to_string(),
        version: None,
        version_relation: None,
        version_scheme: VersionScheme::Rpm,
        architecture_qualifier: ProvideArchitectureQualifier::Implicit,
        provenance: CapabilityProvenance::SourceDerivedFile {
            format: SourcePackageFormat::Rpm,
        },
    };
    let package = IncomingPackage {
        requirements: vec![requirement],
        capabilities: vec![file_provide.clone()],
    };
    let policy = ResolutionPolicy::new()
        .with_mixing(crate::repository::resolution_policy::DependencyMixingPolicy::Permissive);

    // Positive control: the passed File provide discharges the package's own
    // hard PreDepends, so the solve needs nothing external against an empty DB.
    let self_satisfied = solve_package_requirements_with_provides_and_policy(
        &conn,
        &package,
        vec![file_provide],
        &policy,
    )
    .unwrap();
    assert!(
        self_satisfied.conflict_message.is_none(),
        "{self_satisfied:?}"
    );
    assert!(
        self_satisfied.install_order.is_empty(),
        "{self_satisfied:?}"
    );
    assert!(self_satisfied.remove_order.is_empty(), "{self_satisfied:?}");

    // Negative: without that File provide in the passed view the requirement is
    // external, and the empty DB has no provider for it.
    let unresolved =
        solve_package_requirements_with_provides_and_policy(&conn, &package, Vec::new(), &policy)
            .unwrap();
    assert!(unresolved.conflict_message.is_some(), "{unresolved:?}");
    assert!(unresolved.install_order.is_empty(), "{unresolved:?}");
    assert!(unresolved.remove_order.is_empty(), "{unresolved:?}");
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

#[test]
fn strict_known_end_state_includes_the_incoming_package_condition() {
    let (_temp, conn) = setup_test_db();
    let conditional = parse_native_requirement(
        RepositoryRequirementKind::Depends,
        VersionScheme::Rpm,
        "(missing-capability if config(phase4-corpus))",
    )
    .unwrap();
    let policy = ResolutionPolicy::new();

    // The incoming package provides the condition, so the implication is
    // triggered. Its own capability view is part of the fixed end state, so the
    // missing required capability must refuse the solve rather than satisfy the
    // implication vacuously.
    let package = IncomingPackage {
        requirements: vec![conditional],
        capabilities: vec![generic_capability("config(phase4-corpus)")],
    };
    let error = solve_package_requirements_with_provides_outgoing_and_policy(
        &conn,
        &package,
        vec![generic_capability("config(phase4-corpus)")],
        &[],
        &policy,
    )
    .unwrap_err();
    assert!(matches!(error, Error::ConfigError(_)), "{error:?}");

    // Positive control: when the incoming package also provides the required
    // capability, the same conditional requirement holds.
    let satisfied = solve_package_requirements_with_provides_outgoing_and_policy(
        &conn,
        &package,
        vec![
            generic_capability("config(phase4-corpus)"),
            generic_capability("missing-capability"),
        ],
        &[],
        &policy,
    )
    .unwrap();
    assert!(satisfied.install_order.is_empty(), "{satisfied:?}");
    assert_eq!(satisfied.conflict_message, None, "{satisfied:?}");
}

#[test]
fn authority_known_end_state_requires_triggered_incoming_condition() {
    let (_temp, conn) = setup_test_db();
    let repository_id = authority_repository(&conn);
    let conditional = parse_native_requirement(
        RepositoryRequirementKind::Depends,
        VersionScheme::Rpm,
        "(foo if bar)",
    )
    .unwrap();
    let policy = ResolutionPolicy::new().with_primary_source_identity("fedora-44");
    let condition = generic_capability("bar");
    let package = IncomingPackage {
        requirements: vec![conditional],
        capabilities: vec![condition.clone()],
    };

    // The incoming package provides the condition, so the fixed end state makes
    // `bar` present and `foo` required. SAT must see that fact and fail when no
    // admitted `foo` candidate exists instead of discharging the implication
    // vacuously.
    let unresolved = solve_package_requirements_with_provides_outgoing_and_policy(
        &conn,
        &package,
        vec![condition.clone()],
        &[],
        &policy,
    )
    .unwrap();
    assert!(unresolved.conflict_message.is_some(), "{unresolved:?}");
    assert!(unresolved.install_order.is_empty(), "{unresolved:?}");

    // Positive control through the same fixture: admitting `foo` in the
    // repository makes the triggered requirement solvable, and `foo` is the
    // only package the resolution installs.
    insert_repo_pkg_with_reqs(
        &conn,
        repository_id,
        "foo",
        "1.0.0",
        "https://rich-root.invalid/foo.rpm",
        "rpm",
        &[],
    );
    let resolved = solve_package_requirements_with_provides_outgoing_and_policy(
        &conn,
        &package,
        vec![condition],
        &[],
        &policy,
    )
    .unwrap();
    assert!(resolved.conflict_message.is_none(), "{resolved:?}");
    assert_eq!(selected_names(&resolved), ["foo"], "{resolved:?}");
    assert_eq!(resolved.install_order.len(), 1, "{resolved:?}");
}

#[test]
fn authority_known_end_state_counts_incoming_provides_in_sat() {
    let (_temp, conn) = setup_test_db();
    let repository_id = authority_repository(&conn);
    insert_repo_pkg_with_reqs(
        &conn,
        repository_id,
        "foo",
        "1.0.0",
        "https://rich-root.invalid/foo.rpm",
        "rpm",
        &[],
    );
    let conjunction = parse_native_requirement(
        RepositoryRequirementKind::Depends,
        VersionScheme::Rpm,
        "(foo and bar)",
    )
    .unwrap();
    let policy = ResolutionPolicy::new().with_primary_source_identity("fedora-44");
    let condition = generic_capability("bar");
    let package = IncomingPackage {
        requirements: vec![conjunction],
        capabilities: vec![condition.clone()],
    };

    // The incoming package provides `bar` but not `foo`, so only `foo` needs a
    // repository candidate. SAT must see the incoming provide as present rather
    // than require a `bar` solvable that does not exist.
    let resolved = solve_package_requirements_with_provides_outgoing_and_policy(
        &conn,
        &package,
        vec![condition],
        &[],
        &policy,
    )
    .unwrap();
    assert!(resolved.conflict_message.is_none(), "{resolved:?}");
    assert_eq!(selected_names(&resolved), ["foo"], "{resolved:?}");
}
