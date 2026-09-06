// crates/conary-core/src/repository/catalog/parity/tests/rpm_requirements.rs

use super::*;

#[test]
fn packageand_projection_preserves_kind_version_and_source_atom_authority() {
    use super::super::rpm_requirements::native_requirement_groups;
    use crate::repository::dependency_model::RepositoryRequirementKind;
    use crate::repository::rpm_dependency::parse_source_rpm_dependency;

    for (kind, text, expected) in [
        (
            "supplements",
            "packageand(prboom-plus:bash)",
            "(prboom-plus and bash)",
        ),
        (
            "supplements",
            "packageand(:alpha::pattern:beta:)",
            "(alpha and pattern:beta)",
        ),
        ("supplements", "packageand()", "packageand()"),
        (
            "depends",
            "packageand(alpha:beta)",
            "packageand(alpha:beta)",
        ),
        (
            "supplements",
            "packageand(alpha:beta) >= 1",
            "packageand(alpha:beta) >= 1",
        ),
    ] {
        let relation = RepositoryRequirementKind::from_str_exact(kind).unwrap();
        let expression = parse_source_rpm_dependency(relation, text).unwrap();
        let clause = expression.atoms()[0];
        let mut source = requirement(kind, &clause.name);
        source.native_text = Some(text.into());
        source.expression_json = serde_json::to_string(&expression).unwrap();
        source.atoms[0]
            .version_constraint
            .clone_from(&clause.version_constraint);
        source.canonicalize().unwrap();
        let projected =
            native_requirement_groups(VersionScheme::Rpm, vec![source.clone()]).unwrap();
        assert_eq!(projected[0].native_text.as_deref(), Some(expected));
        let projected_expression: RepositoryRequirementExpression =
            serde_json::from_str(&projected[0].expression_json).unwrap();
        assert_eq!(
            projected_expression,
            parse_source_rpm_dependency(relation, expected).unwrap()
        );
        assert_eq!(projected[0].atoms.len(), projected_expression.atoms().len());
        assert_eq!(source.native_text.as_deref(), Some(text));
        let mut damaged = source;
        damaged.atoms[0].capability = "wrong-source-atom".into();
        if text != expected {
            assert!(native_requirement_groups(VersionScheme::Rpm, vec![damaged]).is_err());
        }
    }
}

#[test]
fn rpm_retained_source_epochs_agree_with_strict_stored_expressions() {
    use super::super::rpm_requirements::native_requirement_groups;
    use crate::repository::dependency_model::RepositoryRequirementKind;
    use crate::repository::rpm_dependency::parse_rpm_dependency;

    for (source, canonical, source_only) in [
        ("library = :1.0-23.fc44", "library = 1.0-23.fc44", true),
        (
            "(library = :1.0-23.fc44 if enabled)",
            "(library = 1.0-23.fc44 if enabled)",
            true,
        ),
        (
            "(library = 2:1.0-23.fc44 if enabled)",
            "(library = 2:1.0-23.fc44 if enabled)",
            false,
        ),
    ] {
        let expression =
            parse_rpm_dependency(RepositoryRequirementKind::Depends, canonical).unwrap();
        let mut group = requirement("depends", "library");
        group.expression_json = serde_json::to_string(&expression).unwrap();
        group.native_text = Some(source.into());
        let projected = native_requirement_groups(VersionScheme::Rpm, vec![group.clone()]).unwrap();
        assert_eq!(projected[0].native_text.as_deref(), Some(canonical));
        assert_eq!(projected[0].expression_json, group.expression_json);
        assert_eq!(group.native_text.as_deref(), Some(source));
        if source_only {
            assert!(
                crate::repository::requirement::parse_native_requirement(
                    RepositoryRequirementKind::Depends,
                    VersionScheme::Rpm,
                    source
                )
                .is_err()
            );
        }
    }
}

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
