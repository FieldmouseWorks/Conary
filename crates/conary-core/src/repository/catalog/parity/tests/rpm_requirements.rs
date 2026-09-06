// crates/conary-core/src/repository/catalog/parity/tests/rpm_requirements.rs

use super::*;

#[test]
fn rpm_prerequisite_overlap_preserves_source_and_native_missing_group() {
    let ecosystem = NativeParityEcosystemV1::Rpm;
    let candidate = candidate_resolution::candidate_fixture_with(ecosystem, |_, packages| {
        for package in packages {
            if package.name == "unresolved" {
                package
                    .requirement_groups
                    .push(requirement("depends", "absent"));
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
                    let mut ordinary = requirement("depends", "absent");
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
