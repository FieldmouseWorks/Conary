// crates/conary-core/src/repository/catalog/parity/tests/rpm_provides.rs

use super::super::rpm_provides::native_provides;
use super::*;
use crate::repository::dependency_model::{ProvideVersionRelation, SourcePackageFormat};

fn declared(index: u32) -> CapabilityProvenance {
    CapabilityProvenance::SourceDeclared {
        format: SourcePackageFormat::Rpm,
        record_index: index,
    }
}

#[test]
fn duplicate_source_providers_preserve_native_resolution_and_source_records() {
    let ecosystem = NativeParityEcosystemV1::Rpm;
    let candidate = candidate_resolution::candidate_fixture_with(ecosystem, |_, packages| {
        let package = packages
            .iter_mut()
            .find(|p| p.name == "dependency")
            .unwrap();
        package.provides[0].provenance = declared(0);
        let mut duplicate = package.provides[0].clone();
        duplicate.provenance = declared(1);
        package.provides.push(duplicate);
        let mut later = package.provides[0].clone();
        later.capability = "later-provider".into();
        later.raw = Some("later-provider = 1.0-1".into());
        later.provenance = declared(2);
        package.provides.push(later);
    });
    let mut native_rows = rows(&candidate);
    let dependency = native_rows
        .iter_mut()
        .find(|p| p.name == "dependency")
        .unwrap();
    dependency.provides.retain(|p| p.provenance != declared(1));
    dependency
        .provides
        .iter_mut()
        .find(|p| p.provenance == declared(2))
        .unwrap()
        .provenance = declared(1);
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
        rows(&candidate)
            .iter()
            .find(|p| p.name == "dependency")
            .unwrap()
            .provides
            .len(),
        3
    );
}

#[test]
fn provider_deduplication_preserves_other_facts_and_other_ecosystems() {
    let source = CatalogProvideRecordV1 {
        capability: "library".into(),
        version: Some("1".into()),
        version_relation: Some(ProvideVersionRelation::Equal),
        kind: "generic".into(),
        raw: Some("library = 1".into()),
        version_scheme: VersionScheme::Rpm,
        architecture_qualifier: ProvideArchitectureQualifier::Implicit,
        provenance: declared(0),
    };
    for drift in ["none", "version", "relation", "kind", "raw", "provenance"] {
        let mut duplicate = source.clone();
        duplicate.provenance = declared(1);
        match drift {
            "none" => {}
            "version" => duplicate.version = Some("2".into()),
            "relation" => duplicate.version_relation = Some(ProvideVersionRelation::GreaterThan),
            "kind" => duplicate.kind = "package".into(),
            "raw" => duplicate.raw = Some("distinct retained metadata".into()),
            "provenance" => duplicate.provenance = CapabilityProvenance::ExactIdentity,
            _ => unreachable!(),
        }
        let input = vec![source.clone(), duplicate];
        assert_eq!(
            native_provides(VersionScheme::Rpm, input.clone())
                .unwrap()
                .len(),
            if drift == "none" { 1 } else { 2 },
            "{drift}"
        );
        assert_eq!(
            native_provides(VersionScheme::Debian, input.clone()).unwrap(),
            input
        );
    }
    for invalid in [0, 2] {
        let mut later = source.clone();
        later.provenance = declared(invalid);
        assert!(native_provides(VersionScheme::Rpm, vec![source.clone(), later]).is_err());
    }
}
