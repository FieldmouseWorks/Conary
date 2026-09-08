// apps/conary/src/commands/update/package/tests/summary_capture/fixtures.rs

use super::*;

pub(super) fn add_candidate(
    conn: &rusqlite::Connection,
    dir: &Path,
    name: &str,
    fail: bool,
    relation: bool,
    replacement: Option<&str>,
) {
    let mut bundle = rpm_upgrade_bundle(name, "2.0.0");
    if fail {
        bundle.entries[0].body = "error('forced update summary lifecycle failure')\n".into();
        bundle.entries[0].body_sha256 =
            conary_core::hash::sha256_prefixed(bundle.entries[0].body.as_bytes());
    }
    let artifact_dir = dir.join(name);
    std::fs::create_dir_all(&artifact_dir).unwrap();
    let relations = if let Some(replacement) = replacement {
        vec![
            conary_core::repository::package_relation::parse_native_relation(
                conary_core::repository::dependency_model::RepositoryRequirementKind::Obsolete,
                conary_core::repository::versioning::VersionScheme::Rpm,
                &format!("{replacement} < 2.0.0"),
            )
            .unwrap(),
        ]
    } else if relation {
        use conary_core::repository::{
            dependency_model::RepositoryRequirementKind, versioning::VersionScheme,
        };
        let mut old = Trove::new(
            "summary-obsolete".into(),
            "1".into(),
            TroveType::Package,
            VersionScheme::Debian,
        );
        old.debian_multi_arch =
            Some(conary_core::repository::dependency_model::DebianMultiArch::No);
        old.architecture = Some("amd64".into());
        let obsolete_id = old.insert(conn).unwrap();
        let mut consumer = Trove::new(
            "summary-consumer".into(),
            "1".into(),
            TroveType::Package,
            VersionScheme::Debian,
        );
        consumer.architecture = Some("amd64".into());
        consumer.debian_multi_arch =
            Some(conary_core::repository::dependency_model::DebianMultiArch::No);
        let consumer_id = consumer.insert(conn).unwrap();
        let dependency = conary_core::repository::requirement::parse_native_requirement(
            RepositoryRequirementKind::Depends,
            VersionScheme::Debian,
            "summary-obsolete",
        )
        .unwrap();
        conary_core::db::models::InstalledRequirementGroup::insert_groups(
            conn,
            consumer_id,
            VersionScheme::Debian,
            &[dependency],
        )
        .unwrap();
        let mut consumer_bundle = rpm_upgrade_bundle("summary-consumer", "1");
        consumer_bundle.source_format = SourceFormat::Deb;
        consumer_bundle.source_family = "debian".into();
        consumer_bundle.source_profile = Some("ubuntu-26.04".into());
        consumer_bundle.source_release = Some("26.04".into());
        consumer_bundle.source_arch = Some("amd64".into());
        consumer_bundle.evidence_digest = None;
        consumer_bundle.version_scheme = conary_core::ccs::native_lifecycle::VersionScheme::Deb;
        consumer_bundle.entries.clear();
        let mut obsolete_bundle = consumer_bundle.clone();
        obsolete_bundle.source_package = "summary-obsolete".into();
        conary_core::db::models::InstalledNativeLifecycleBundle::new(
            obsolete_id,
            None,
            &obsolete_bundle,
        )
        .unwrap()
        .insert_or_replace(conn)
        .unwrap();
        conary_core::db::models::InstalledNativeLifecycleBundle::new(
            consumer_id,
            None,
            &consumer_bundle,
        )
        .unwrap()
        .insert_or_replace(conn)
        .unwrap();
        vec![
            conary_core::repository::package_relation::parse_native_relation(
                RepositoryRequirementKind::Obsolete,
                VersionScheme::Rpm,
                "summary-obsolete",
            )
            .unwrap(),
        ]
    } else {
        Vec::new()
    };
    let path = build_test_ccs_package_with_relations(
        &artifact_dir,
        name,
        "2.0.0",
        Some(bundle),
        relations,
    );
    let bytes = std::fs::read(&path).unwrap();
    let (url, _) = serve_test_file(path);
    let repo = insert_test_static_ccs_repository(conn, name, &url);
    let mut old = Trove::new_with_source(
        name.into(),
        "1.0.0".into(),
        TroveType::Package,
        InstallSource::Repository,
        conary_core::repository::versioning::VersionScheme::Rpm,
    );
    old.architecture = Some("x86_64".into());
    old.source_profile = Some("fedora-44".into());
    old.installed_from_repository_id = Some(repo);
    old.insert(conn).unwrap();
    let mut candidate = RepositoryPackage::new(
        repo,
        name.into(),
        "2.0.0".into(),
        conary_core::repository::versioning::VersionScheme::Rpm,
        conary_core::hash::sha256(&bytes),
        bytes.len() as i64,
        url,
    );
    candidate.architecture = Some("x86_64".into());
    candidate.source_profile = Some("fedora-44".into());
    let id = candidate.insert(conn).unwrap();
    let mut resolution = PackageResolution::new(
        repo,
        name.into(),
        vec![ResolutionStrategy::RepositoryPackage {
            repository_package_id: id,
        }],
    );
    resolution.version = Some("2.0.0".into());
    resolution.primary_strategy = PrimaryStrategy::RepositoryPackage;
    resolution.insert(conn).unwrap();
}
