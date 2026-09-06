// crates/conary-core/src/repository/catalog/parity/tests/rpm_epochs.rs

use super::*;
use crate::repository::dependency_model::RepositoryRequirementKind;
use crate::repository::rpm_dependency::parse_source_rpm_dependency;

#[test]
fn zero_epoch_projection_preserves_source_and_native_missing_group() {
    let ecosystem = NativeParityEcosystemV1::Rpm;
    let candidate = candidate_resolution::candidate_fixture_with(ecosystem, |_, packages| {
        let group = &mut packages
            .iter_mut()
            .find(|p| p.name == "unresolved")
            .unwrap()
            .requirement_groups[0];
        let expression =
            parse_source_rpm_dependency(RepositoryRequirementKind::PreDepends, "absent >= 0:1")
                .unwrap();
        group.expression_json = serde_json::to_string(&expression).unwrap();
        group.native_text = Some("absent >= 0:1".into());
        group.atoms[0].version_constraint = Some(">= 0:1".into());
    });
    let mut native_rows = rows(&candidate);
    let unresolved = native_rows
        .iter_mut()
        .find(|row| row.name == "unresolved")
        .unwrap();
    let group = &mut unresolved.requirement_groups[0];
    group.expression_json = serde_json::to_string(
        &parse_source_rpm_dependency(RepositoryRequirementKind::PreDepends, "absent >= 1").unwrap(),
    )
    .unwrap();
    group.native_text = Some("absent >= 1".into());
    group.atoms[0].version_constraint = Some(">= 1".into());
    group.canonicalize().unwrap();
    let expected = NativeResolutionOutcomeV1::Unresolved {
        dependencies: vec![NativeUnresolvedDependencyV1 {
            requiring_package_key_sha256: unresolved.package_key_sha256.clone(),
            requirement_group_sha256: native_requirement_group_sha256(group).unwrap(),
        }],
    };
    let package_oracle = oracle(&candidate, ecosystem, native_rows);
    let output = tempfile::tempdir().unwrap();
    let survey = produce_conary_resolution_survey(
        &candidate.profile,
        &candidate.reader,
        package_oracle._directory.path(),
        "x86_64",
        &output.path().join("survey.json"),
    )
    .unwrap();
    assert_eq!(survey.counts.failed_roots, 0);
    assert_eq!(
        survey
            .outcomes
            .iter()
            .find(|row| row.name == "unresolved")
            .unwrap()
            .outcome,
        expected
    );
    let persisted = rows(&candidate);
    let group = &persisted
        .iter()
        .find(|row| row.name == "unresolved")
        .unwrap()
        .requirement_groups[0];
    assert_eq!(group.native_text.as_deref(), Some("absent >= 0:1"));
    assert_eq!(group.atoms[0].version_constraint.as_deref(), Some(">= 0:1"));
}

#[test]
fn zero_epochs_project_every_operand_without_changing_positive_epochs_or_metadata() {
    use super::super::rpm_requirements::native_requirement_groups;
    for (kind, source_text, expected_text) in [
        (RepositoryRequirementKind::Depends, "a = 0:1", "a = 1"),
        (
            RepositoryRequirementKind::Depends,
            "(a >= 0:1 and b <= 0:2)",
            "(a >= 1 and b <= 2)",
        ),
        (
            RepositoryRequirementKind::Depends,
            "(a > 0:1 or b < 0:2)",
            "(a > 1 or b < 2)",
        ),
        (
            RepositoryRequirementKind::Depends,
            "(a = 0:1 if b = 0:2 else c = 0:3)",
            "(a = 1 if b = 2 else c = 3)",
        ),
        (
            RepositoryRequirementKind::Conflict,
            "(a = 0:1 unless b = 0:2 else c = 0:3)",
            "(a = 1 unless b = 2 else c = 3)",
        ),
        (
            RepositoryRequirementKind::Depends,
            "(a = 0:1 with b = 0:2)",
            "(a = 1 with b = 2)",
        ),
        (
            RepositoryRequirementKind::Depends,
            "(a = 0:1 without b = 2:2)",
            "(a = 1 without b = 2:2)",
        ),
    ] {
        let expression = parse_source_rpm_dependency(kind, source_text).unwrap();
        let mut source = requirement(kind.as_str(), "a");
        let template = source.atoms[0].clone();
        source.atoms = expression
            .atoms()
            .into_iter()
            .map(|clause| {
                let mut atom = template.clone();
                atom.capability.clone_from(&clause.name);
                atom.version_constraint
                    .clone_from(&clause.version_constraint);
                atom.raw = Some("retained atom metadata".into());
                atom
            })
            .collect();
        source.expression_json = serde_json::to_string(&expression).unwrap();
        source.native_text = Some(source_text.into());
        source.canonicalize().unwrap();
        let projected =
            native_requirement_groups(VersionScheme::Rpm, vec![source.clone()]).unwrap();
        let expected = parse_source_rpm_dependency(kind, expected_text).unwrap();
        assert_eq!(projected[0].native_text.as_deref(), Some(expected_text));
        assert_eq!(
            serde_json::from_str::<RepositoryRequirementExpression>(&projected[0].expression_json)
                .unwrap(),
            expected
        );
        for atom in &projected[0].atoms {
            assert!(
                expected
                    .atoms()
                    .iter()
                    .any(|clause| clause.name == atom.capability
                        && clause.version_constraint == atom.version_constraint)
            );
            assert_eq!(atom.raw.as_deref(), Some("retained atom metadata"));
        }
        assert_eq!(
            native_requirement_groups(VersionScheme::Debian, vec![source.clone()]).unwrap(),
            vec![source]
        );
    }
}
