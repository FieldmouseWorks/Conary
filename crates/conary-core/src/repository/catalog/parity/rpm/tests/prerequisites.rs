// crates/conary-core/src/repository/catalog/parity/rpm/tests/prerequisites.rs

use super::*;

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
