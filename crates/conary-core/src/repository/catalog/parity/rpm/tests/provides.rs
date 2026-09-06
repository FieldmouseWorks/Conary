// crates/conary-core/src/repository/catalog/parity/rpm/tests/provides.rs

use super::*;

#[test]
fn source_provider_duplicates_match_native_ordered_identity_set() {
    let directory = tempfile::tempdir().unwrap();
    let checksum = digest('a');
    let mut package = PackageFixture::simple("provider-overlap", &checksum);
    package.format = r#"<rpm:provides>
        <rpm:entry name="alpha"/><rpm:entry name="beta"/>
        <rpm:entry name="alpha"/><rpm:entry name="alpha" flags="GE" ver="2"/>
        <rpm:entry name="alpha" flags="GE" ver="2"/>
        <rpm:entry name="beta"/><rpm:entry name="gamma"/>
        <rpm:entry name="provider-overlap" flags="EQ" ver="1.0" rel="1.fc44"/>
    </rpm:provides>"#;
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
    reader
        .for_each_package(|row| {
            let mut declarations = Vec::new();
            let mut source = Vec::new();
            for provide in &row.provides {
                if let CapabilityProvenance::SourceDeclared {
                    format: SourcePackageFormat::Rpm,
                    record_index,
                } = provide.provenance
                {
                    declarations.push((record_index, provide.clone()));
                } else {
                    source.push(provide.clone());
                }
            }
            declarations.sort_by_key(|(index, _)| *index);
            assert_eq!(declarations.len(), 5);
            for (source_index, native_index) in [0, 1, 0, 2, 2, 1, 3, 4].into_iter().enumerate() {
                let mut provide = declarations[native_index].1.clone();
                provide.provenance = CapabilityProvenance::SourceDeclared {
                    format: SourcePackageFormat::Rpm,
                    record_index: u32::try_from(source_index).unwrap(),
                };
                source.push(provide);
            }
            let projected = crate::repository::catalog::parity::rpm_provides::native_provides(
                VersionScheme::Rpm,
                source,
            )?;
            assert_eq!(projected, row.provides);
            Ok(())
        })
        .unwrap();
}
