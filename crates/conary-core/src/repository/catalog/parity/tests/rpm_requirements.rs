// crates/conary-core/src/repository/catalog/parity/tests/rpm_requirements.rs

use super::*;

#[test]
fn rpm_rich_spelling_is_projected_only_after_typed_agreement() {
    use super::super::rpm_requirements::native_requirement_groups;
    use crate::repository::dependency_model::RepositoryRequirementKind;
    use crate::repository::rpm_dependency::parse_rpm_dependency;

    let source_text = "((linux-firmware = 20260810-1.fc44) if linux-firmware)";
    let native_text = "(linux-firmware = 20260810-1.fc44 if linux-firmware)";
    let expression = parse_rpm_dependency(RepositoryRequirementKind::Depends, source_text).unwrap();
    let mut source = requirement("depends", "linux-firmware");
    source.behavior = "conditional".into();
    source.native_text = Some(source_text.into());
    source.expression_json = serde_json::to_string(&expression).unwrap();
    source.atoms.push(source.atoms[0].clone());
    source.atoms[0].version_constraint = Some("= 20260810-1.fc44".into());
    let mut native = source.clone();
    native.native_text = Some(native_text.into());
    // Different source spellings of the same relation also share one native ID.
    let projected =
        native_requirement_groups(VersionScheme::Rpm, vec![source.clone(), native.clone()])
            .unwrap();
    assert_eq!(projected, vec![native]);
    assert_eq!(source.native_text.as_deref(), Some(source_text));
    assert_eq!(
        native_requirement_groups(VersionScheme::Debian, vec![source.clone()]).unwrap(),
        vec![source.clone()]
    );

    source.native_text = Some("(linux-firmware = 20260811-1.fc44 if linux-firmware)".into());
    assert!(native_requirement_groups(VersionScheme::Rpm, vec![source]).is_err());
}

#[test]
fn rpm_prerequisite_overlap_preserves_source_and_native_missing_group() {
    let ecosystem = NativeParityEcosystemV1::Rpm;
    let candidate = candidate_resolution::candidate_fixture_with(ecosystem, |_, packages| {
        for package in packages {
            if package.name == "unresolved" {
                let mut ordinary = package.requirement_groups[0].clone();
                ordinary.kind = "depends".into();
                package.requirement_groups.push(ordinary);
            }
        }
    });
    let mut native_rows = rows(&candidate);
    let unresolved = native_rows
        .iter_mut()
        .find(|row| row.name == "unresolved")
        .unwrap();
    // libsolv 0.7.36 repo_addid_dep retains only the prerequisite for one
    // dependency ID, regardless of which declaration occurred first.
    unresolved
        .requirement_groups
        .retain(|group| group.kind != "depends");
    let expected = NativeResolutionOutcomeV1::Unresolved {
        dependencies: vec![NativeUnresolvedDependencyV1 {
            requiring_package_key_sha256: unresolved.package_key_sha256.clone(),
            requirement_group_sha256: native_requirement_group_sha256(
                &unresolved.requirement_groups[0],
            )
            .unwrap(),
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
    assert_eq!(survey.counts.resolved_roots, 2);
    assert_eq!(survey.counts.unresolved_roots, 1);
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
    assert_eq!(
        persisted
            .iter()
            .find(|row| row.name == "unresolved")
            .unwrap()
            .requirement_groups
            .len(),
        2
    );
}

#[test]
fn prerequisite_overlap_does_not_hide_other_facts_or_non_rpm_declarations() {
    for ecosystem in [
        NativeParityEcosystemV1::Rpm,
        NativeParityEcosystemV1::Debian,
        NativeParityEcosystemV1::Alpm,
    ] {
        for drift in [
            "none",
            "version",
            "description",
            "behavior",
            "native_text",
            "atom",
        ] {
            let candidate =
                candidate_resolution::candidate_fixture_with(ecosystem, |_, packages| {
                    let package = packages
                        .iter_mut()
                        .find(|row| row.name == "unresolved")
                        .unwrap();
                    let mut ordinary = package.requirement_groups[0].clone();
                    ordinary.kind = "depends".into();
                    match drift {
                        "none" => {}
                        "version" => {
                            ordinary.expression_json =
                                serde_json::to_string(&RepositoryRequirementExpression::Atom(
                                    RepositoryRequirementClause::versioned(
                                        "absent".into(),
                                        ">= 2".into(),
                                    ),
                                ))
                                .unwrap();
                            ordinary.atoms[0].version_constraint = Some(">= 2".into());
                            ordinary.native_text = Some("absent >= 2".into());
                        }
                        "description" => ordinary.description = Some("distinct declaration".into()),
                        "behavior" => ordinary.behavior = "conditional".into(),
                        "native_text" => ordinary.native_text = Some("distinct native text".into()),
                        "atom" => ordinary.atoms[0].raw = Some("distinct atom text".into()),
                        _ => unreachable!(),
                    }
                    package.requirement_groups.push(ordinary);
                });
            let mut native_rows = rows(&candidate);
            native_rows
                .iter_mut()
                .find(|row| row.name == "unresolved")
                .unwrap()
                .requirement_groups
                .retain(|group| group.kind != "depends");
            let package_oracle = oracle(&candidate, ecosystem, native_rows);
            let result = compare_native_parity_oracle(
                &candidate.profile,
                &candidate.reader,
                &package_oracle.reader,
            );
            if ecosystem == NativeParityEcosystemV1::Rpm && drift == "none" {
                result.unwrap();
            } else {
                assert!(result.is_err(), "{ecosystem:?} {drift}: {result:?}");
            }
        }
    }
}
