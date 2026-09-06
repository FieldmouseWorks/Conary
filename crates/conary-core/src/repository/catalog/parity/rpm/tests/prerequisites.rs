// crates/conary-core/src/repository/catalog/parity/rpm/tests/prerequisites.rs

use super::*;

#[test]
fn source_zero_epoch_projection_matches_native_expression_and_atom_index() {
    for (kind, tag, source_text) in [
        (
            RepositoryRequirementKind::Depends,
            "requires",
            "(library = 0:1.0 if enabled)",
        ),
        (
            RepositoryRequirementKind::Depends,
            "requires",
            "(a >= 0:1 and b <= 0:2)",
        ),
        (
            RepositoryRequirementKind::Depends,
            "requires",
            "(a > 0:1 or b < 0:2)",
        ),
        (
            RepositoryRequirementKind::Depends,
            "requires",
            "(a = 0:1 if b = 0:2 else c = 0:3)",
        ),
        (
            RepositoryRequirementKind::Conflict,
            "conflicts",
            "(a = 0:1 unless b = 0:2 else c = 0:3)",
        ),
        (
            RepositoryRequirementKind::Depends,
            "requires",
            "(a = 0:1 with b = 0:2)",
        ),
        (
            RepositoryRequirementKind::Depends,
            "requires",
            "(a = 0:1 without b = 2:2)",
        ),
        (
            RepositoryRequirementKind::Depends,
            "requires",
            "(a = 0:1 and a = 1)",
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let checksum = digest('a');
        let escaped = source_text.replace('<', "&lt;").replace('>', "&gt;");
        let format = format!("<rpm:{tag}><rpm:entry name=\"{escaped}\"/></rpm:{tag}>");
        let mut package = PackageFixture::simple("zero-epoch-projection", &checksum);
        package.format = &format;
        let metadata = vec![write_metadata(directory.path(), "fedora-core", &[package])];
        let snapshots = vec![source_snapshot(
            "fedora-core",
            &metadata[0].0,
            &metadata[0].1,
        )];
        let mut profile = profile(&snapshots);
        profile.counts.packages = 1;
        let output = directory.path().join("oracle");
        produce_rpm_parity_oracle(&profile, &inputs(&snapshots, &metadata), &output).unwrap();
        let reader = verify_native_parity_oracle_bundle(&output, &profile).unwrap();
        reader.for_each_package(|row| {
            assert_eq!(row.requirement_groups.len(), 1);
            let mut source = row.requirement_groups[0].clone();
            let expression = crate::repository::rpm_dependency::parse_source_rpm_dependency(
                kind, source_text,
            ).unwrap();
            let template = source.atoms[0].clone();
            source.atoms = expression.atoms().into_iter().map(|clause| {
                let mut atom = template.clone();
                atom.capability.clone_from(&clause.name);
                atom.version_constraint.clone_from(&clause.version_constraint);
                atom
            }).collect();
            source.expression_json = serde_json::to_string(&expression).unwrap();
            source.native_text = Some(source_text.into());
            source.canonicalize()?;
            let projected = crate::repository::catalog::parity::rpm_requirements::native_requirement_groups(
                VersionScheme::Rpm, vec![source],
            )?;
            assert_eq!(projected, row.requirement_groups, "{source_text}");
            Ok(())
        }).unwrap();
    }
}

#[test]
fn source_supplement_projection_matches_pinned_packageand_grammar() {
    for source_text in [
        "packageand(prboom-plus:bash)".to_string(),
        "packageand(openqa:postgresql-server)".to_string(),
        "packageand(:alpha::pattern:beta:)".to_string(),
        "packageand()".to_string(),
        format!("packageand({}:b)", "a".repeat(1009)),
        format!("packageand({}:b)", "a".repeat(1010)),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let checksum = digest('a');
        let format =
            format!("<rpm:supplements><rpm:entry name=\"{source_text}\"/></rpm:supplements>");
        let mut package = PackageFixture::simple("supplement-projection", &checksum);
        package.format = &format;
        let metadata = vec![write_metadata(directory.path(), "fedora-core", &[package])];
        let snapshots = vec![source_snapshot(
            "fedora-core",
            &metadata[0].0,
            &metadata[0].1,
        )];
        let mut profile = profile(&snapshots);
        profile.counts.packages = 1;
        let output = directory.path().join("oracle");
        produce_rpm_parity_oracle(&profile, &inputs(&snapshots, &metadata), &output).unwrap();
        let reader = verify_native_parity_oracle_bundle(&output, &profile).unwrap();
        reader.for_each_package(|row| {
            assert_eq!(row.requirement_groups.len(), 1);
            let mut source = row.requirement_groups[0].clone();
            let expression = crate::repository::rpm_dependency::parse_source_rpm_dependency(RepositoryRequirementKind::Supplements, &source_text).unwrap();
            source.expression_json = serde_json::to_string(&expression).unwrap();
            source.native_text = Some(source_text.clone());
            source.atoms.truncate(1);
            source.atoms[0].capability = source_text.clone();
            source.canonicalize()?;
            let projected = crate::repository::catalog::parity::rpm_requirements::native_requirement_groups(VersionScheme::Rpm, vec![source])?;
            assert_eq!(projected, row.requirement_groups, "{source_text}");
            Ok(())
        }).unwrap();
    }
}

#[test]
fn native_prerequisite_precedence_is_independent_of_source_order() {
    let cases = [
        (
            r#"<rpm:entry name="setup"/><rpm:entry name="setup" pre="1"/>"#,
            1,
        ),
        (
            r#"<rpm:entry name="setup" pre="1"/><rpm:entry name="setup"/>"#,
            1,
        ),
        (
            r#"<rpm:entry name="setup" flags="GE" ver="2"/><rpm:entry name="setup" pre="1"/>"#,
            2,
        ),
        (
            r#"<rpm:entry name="((setup &gt;= 2) if enabled)"/><rpm:entry name="(setup &gt;= 2 if enabled)" pre="1"/>"#,
            1,
        ),
    ];
    for (requires, count) in cases {
        let directory = tempfile::tempdir().unwrap();
        let checksum = digest('a');
        let format = format!("<rpm:requires>{requires}</rpm:requires>");
        let mut package = PackageFixture::simple("prerequisite-overlap", &checksum);
        package.format = &format;
        let metadata = vec![write_metadata(directory.path(), "fedora-core", &[package])];
        let snapshots = vec![source_snapshot(
            "fedora-core",
            &metadata[0].0,
            &metadata[0].1,
        )];
        let mut profile = profile(&snapshots);
        profile.counts.packages = 1;
        let output = directory.path().join("oracle");
        produce_rpm_parity_oracle(&profile, &inputs(&snapshots, &metadata), &output).unwrap();
        let reader = verify_native_parity_oracle_bundle(&output, &profile).unwrap();
        reader.for_each_package(|row| {
            assert_eq!(row.requirement_groups.len(), count, "{requires}");
            assert_eq!(row.requirement_groups.iter().filter(|group| group.kind == "pre_depends").count(), 1);
            if count == 1 {
                let mut ordinary = row.requirement_groups[0].clone();
                ordinary.kind = "depends".into();
                let mut source = row.requirement_groups.clone();
                source.push(ordinary);
                let projected = crate::repository::catalog::parity::rpm_requirements::native_requirement_groups(VersionScheme::Rpm, source)?;
                assert_eq!(projected, row.requirement_groups);
            }
            Ok(())
        }).unwrap();
    }
}
