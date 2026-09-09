// apps/conary/src/commands/update/package/tests/summary_capture/cancellation.rs

use super::*;
use crate::commands::test_helpers::native_artifact::{detached_signature, pgp_authority};
use conary_core::db::models::{
    NativeSourceEcosystem, NativeSourceStream, RepositoryPolicyScope, RepositorySourcePolicy,
    RepositoryUpdateMode,
};
use conary_core::repository::versioning::VersionScheme;
use conary_core::repository::{
    RepositoryParserConfig, RepositoryTrustPolicy, RpmMetadataAuthority,
};

pub(super) async fn add_candidate(
    conn: &rusqlite::Connection,
    dir: &Path,
    db_path: &str,
    scenario: &str,
) {
    let authority = pgp_authority(dir);
    let mut builder = rpm::PackageBuilder::new(
        "a-summary-update",
        "2.0.0",
        "MIT",
        "x86_64",
        "cancel update fixture",
    );
    builder.requires(rpm::Dependency::any("summary-dependency"));
    builder
        .with_file_contents(
            b"updated payload".to_vec(),
            rpm::FileOptions::new("/usr/share/a-summary-update/data").permissions(0o644),
        )
        .unwrap();
    let mut package = builder.build().unwrap();
    package
        .apply_signature(detached_signature(
            &authority.certificate,
            &package.header_bytes().unwrap(),
        ))
        .unwrap();
    let path = dir.join("cancel-update.rpm");
    package.write_file(&path).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let (url, _) = serve_test_file(path);
    let mut repo = Repository::new(
        "cancel-native".into(),
        "https://example.invalid/fixture".into(),
    );
    repo.set_parser_config(RepositoryParserConfig::Rpm {
        architecture: "x86_64".into(),
    })
    .unwrap();
    repo.set_trust_policy(RepositoryTrustPolicy::Rpm {
        metadata: RpmMetadataAuthority::OpenPgp {
            keys: vec![authority.root.clone()],
        },
        package_keys: vec![authority.root],
    })
    .unwrap();
    repo.set_native_source_policy(
        RepositorySourcePolicy::new(
            "fedora-44",
            RepositoryPolicyScope::repository("fedora-44").unwrap(),
            NativeSourceEcosystem::Rpm,
            NativeSourceStream::channel("stable").unwrap(),
            RepositoryUpdateMode::Follow,
        )
        .unwrap(),
        "fedora-44",
        None,
    )
    .unwrap();
    repo.source_profile = Some("fedora-44".into());
    let repo_id = repo.insert(conn).unwrap();
    conary_core::repository::trust::openpgp::PreparedOpenPgpTrust::prepare(
        &repo.name,
        &conary_core::db::paths::keyring_dir(db_path),
        repo.require_trust_policy().unwrap(),
    )
    .await
    .unwrap();
    let mut old = Trove::new_with_source(
        "a-summary-update".into(),
        "1.0.0".into(),
        TroveType::Package,
        InstallSource::Repository,
        VersionScheme::Rpm,
    );
    old.architecture = Some("x86_64".into());
    old.source_profile = Some("fedora-44".into());
    old.installed_from_repository_id = Some(repo_id);
    old.insert(conn).unwrap();
    let mut candidate = RepositoryPackage::new(
        repo_id,
        "a-summary-update".into(),
        "2.0.0-1".into(),
        VersionScheme::Rpm,
        conary_core::hash::sha256(&bytes),
        bytes.len() as i64,
        url,
    );
    candidate.architecture = Some("x86_64".into());
    candidate.source_profile = Some("fedora-44".into());
    let id = candidate.insert(conn).unwrap();
    let mut resolution = PackageResolution::new(
        repo_id,
        candidate.name.clone(),
        vec![ResolutionStrategy::RepositoryPackage {
            repository_package_id: id,
        }],
    );
    resolution.version = Some(candidate.version.clone());
    resolution.primary_strategy = PrimaryStrategy::RepositoryPackage;
    resolution.insert(conn).unwrap();

    let dep_dir = dir.join("cancel-dependency");
    std::fs::create_dir_all(&dep_dir).unwrap();
    let dependency = build_test_ccs_package_with_owners(
        &dep_dir,
        "summary-dependency",
        "1.0.0",
        Some(rpm_upgrade_bundle("summary-dependency", "1.0.0")),
        Vec::new(),
        false,
    );
    let dep_bytes = std::fs::read(&dependency).unwrap();
    let dep_repo = insert_test_static_ccs_repository(
        conn,
        "cancel-dependencies",
        "https://example.invalid/fixture",
    );
    let mut dep = RepositoryPackage::new(
        dep_repo,
        "summary-dependency".into(),
        "1.0.0".into(),
        VersionScheme::Rpm,
        conary_core::hash::sha256(&dep_bytes),
        dep_bytes.len() as i64,
        url::Url::from_file_path(&dependency).unwrap().to_string(),
    );
    dep.architecture = Some("x86_64".into());
    dep.source_profile = Some("fedora-44".into());
    let dep_id = dep.insert(conn).unwrap();
    conary_core::db::models::RepositoryProvide::new(
        dep_id,
        dep.name.clone(),
        Some(dep.version.clone()),
        "package".into(),
        None,
        VersionScheme::Rpm,
    )
    .insert(conn)
    .unwrap();
    fixtures::add_candidate(conn, dir, "z-summary-update", None, false, None, false);
    if scenario != "cancel_full" {
        let objects = conary_core::db::paths::objects_dir(db_path);
        let cas = conary_core::filesystem::CasStore::new(&objects).unwrap();
        let from_hash = cas.store(b"old package bytes").unwrap();
        let to_hash = cas.store(&bytes).unwrap();
        for hash in [&from_hash, &to_hash] {
            conn.execute(
                "INSERT INTO file_contents (sha256_hash, content_path, size) VALUES (?1, ?2, 0)",
                rusqlite::params![hash, format!("objects/{hash}")],
            )
            .unwrap();
        }
        let path = dir.join("cancel.delta");
        conary_core::delta::DeltaGenerator::new(&objects)
            .unwrap()
            .generate_delta(&from_hash, &to_hash, &path)
            .unwrap();
        let delta = std::fs::read(&path).unwrap();
        let (url, _) = serve_test_file(path);
        let checksum = conary_core::hash::sha256(if scenario == "cancel_fallback" {
            b"bad checksum"
        } else {
            &delta
        });
        PackageDelta::new(
            candidate.name,
            "1.0.0".into(),
            candidate.version,
            from_hash,
            to_hash,
            url,
            delta.len() as i64,
            checksum,
            bytes.len() as i64,
        )
        .insert(conn)
        .unwrap();
    }
}
