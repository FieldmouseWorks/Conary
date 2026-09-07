// crates/conary-core/src/resolver/sat/tests/discovery.rs

use super::*;
use crate::repository::dependency_model::RepositoryRequirementKind;
use crate::repository::requirement::parse_native_requirement;

fn fixture() -> (tempfile::TempDir, Connection, i64) {
    let (temp, conn) = setup_test_db();
    let mut repo = Repository::new("discovery".into(), "https://example.invalid".into());
    let repo_id = repo.insert(&conn).unwrap();
    (temp, conn, repo_id)
}

fn expression(conn: &Connection, package: i64, kind: RepositoryRequirementKind, text: &str) {
    let requirement = match kind {
        RepositoryRequirementKind::Depends => {
            parse_native_requirement(kind, VersionScheme::Rpm, text)
        }
        RepositoryRequirementKind::Conflict => {
            crate::repository::package_relation::parse_native_relation(
                kind,
                VersionScheme::Rpm,
                text,
            )
        }
        _ => unreachable!("fixture uses dependencies and conflicts"),
    }
    .unwrap();
    insert_typed_repo_requirement_group(conn, package, &requirement);
}

fn candidates(conn: &Connection, roots: &[&str]) -> std::collections::BTreeSet<String> {
    let policy = ResolutionPolicy::new().with_primary_source_identity("fedora-44");
    let requests = roots
        .iter()
        .map(|name| ((*name).to_string(), VersionConstraint::Any))
        .collect::<Vec<_>>();
    let provider = install::build_provider_for_install(conn, &requests, &policy).unwrap();
    provider
        .solvable_ids()
        .iter()
        .map(|id| provider.get_solvable(*id).name.clone())
        .collect()
}

#[test]
fn negative_relation_does_not_expand_an_unrequested_dependency_tree() {
    let (_temp, conn, repo) = fixture();
    let root = insert_rpm_repo_package(&conn, repo, "root", "1");
    let excluded = insert_rpm_repo_package(&conn, repo, "excluded", "1");
    insert_rpm_repo_package(&conn, repo, "unrelated", "1");
    expression(&conn, root, RepositoryRequirementKind::Conflict, "excluded");
    expression(
        &conn,
        excluded,
        RepositoryRequirementKind::Depends,
        "unrelated",
    );
    assert_eq!(candidates(&conn, &["root"]), ["root".to_string()].into());
    let alone = solve_install(&conn, &[("root".into(), VersionConstraint::Any)]).unwrap();
    assert!(alone.conflict_message.is_none(), "{alone:?}");
    let together = solve_install(
        &conn,
        &[
            ("root".into(), VersionConstraint::Any),
            ("excluded".into(), VersionConstraint::Any),
        ],
    )
    .unwrap();
    assert!(together.conflict_message.is_some(), "{together:?}");
}

#[test]
fn conditional_guard_is_discovered_when_a_positive_path_requires_it() {
    let (_temp, conn, repo) = fixture();
    let root = insert_rpm_repo_package(&conn, repo, "root", "1");
    let guard = insert_rpm_repo_package(&conn, repo, "guard", "1");
    insert_rpm_repo_package(&conn, repo, "guard-runtime", "1");
    expression(
        &conn,
        root,
        RepositoryRequirementKind::Depends,
        "(missing if guard)",
    );
    expression(
        &conn,
        guard,
        RepositoryRequirementKind::Depends,
        "guard-runtime",
    );
    assert_eq!(candidates(&conn, &["root"]), ["root".to_string()].into());
    let alone = solve_install(&conn, &[("root".into(), VersionConstraint::Any)]).unwrap();
    assert!(alone.conflict_message.is_none(), "{alone:?}");
    let together = solve_install(
        &conn,
        &[
            ("root".into(), VersionConstraint::Any),
            ("guard".into(), VersionConstraint::Any),
        ],
    )
    .unwrap();
    assert!(together.conflict_message.is_some(), "{together:?}");
}

#[test]
fn nested_root_negation_preserves_positive_literals() {
    use crate::resolver::provider::{ConaryConstraint, SolverExpression};
    let (_temp, conn, repo) = fixture();
    insert_rpm_repo_package(&conn, repo, "guard", "1");
    insert_rpm_repo_package(&conn, repo, "excluded", "1");
    let atom = |name: &str| {
        SolverExpression::atom(
            name.to_string(),
            ConaryConstraint::Requested(VersionConstraint::Any),
        )
    };
    let root = SolverExpression::Not(Box::new(SolverExpression::Or(vec![
        SolverExpression::Not(Box::new(atom("guard"))),
        atom("excluded"),
    ])));
    let policy = ResolutionPolicy::new().with_primary_source_identity("fedora-44");
    let provider =
        install::build_provider_for_requirement_expressions(&conn, &[root], &policy).unwrap();
    let names = provider
        .solvable_ids()
        .iter()
        .map(|id| provider.get_solvable(*id).name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["guard"]);
}
