// apps/conary/src/commands/install/repository_batch/tests.rs

use super::*;
use crate::commands::test_helpers::native_artifact::{pgp_authority, signed_rpm_bytes};
use conary_core::db::models::{
    NativeSourceEcosystem, NativeSourceStream, Repository, RepositoryPackage,
    RepositoryPolicyScope, RepositorySourcePolicy, RepositoryUpdateMode,
};
use conary_core::repository::trust::openpgp::PreparedOpenPgpTrust;
use conary_core::repository::versioning::VersionScheme;
use conary_core::repository::{
    RepositoryParserConfig, RepositoryTrustPolicy, RpmMetadataAuthority,
};
use conary_core::resolver::{SatPackage, SatSource};

#[tokio::test]
async fn projected_native_dependency_uses_runtime_keyring_without_mutation() {
    let (temp, db_path) = crate::commands::test_helpers::create_test_db();
    let conn = conary_core::db::open(&db_path).unwrap();
    let authority = pgp_authority(temp.path());
    let bytes = signed_rpm_bytes(&authority);
    let served = bytes.clone();
    let app = axum::Router::new().route(
        "/exact.rpm",
        axum::routing::get(move || {
            let bytes = served.clone();
            async move { bytes }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut repository = Repository::new("preview-native".into(), format!("http://{address}"));
    repository
        .set_parser_config(RepositoryParserConfig::Rpm {
            architecture: "x86_64".into(),
        })
        .unwrap();
    repository
        .set_trust_policy(RepositoryTrustPolicy::Rpm {
            metadata: RpmMetadataAuthority::OpenPgp {
                keys: vec![authority.root.clone()],
            },
            package_keys: vec![authority.root],
        })
        .unwrap();
    repository
        .set_native_source_policy(
            RepositorySourcePolicy::new(
                "preview-native",
                RepositoryPolicyScope::repository("fedora-44").unwrap(),
                NativeSourceEcosystem::Rpm,
                NativeSourceStream::channel("stable").unwrap(),
                RepositoryUpdateMode::Follow,
            )
            .unwrap(),
            "fedora-44".into(),
            Some("fedora-44".into()),
        )
        .unwrap();
    let repo_id = repository.insert(&conn).unwrap();
    PreparedOpenPgpTrust::prepare(
        &repository.name,
        &keyring_dir(&db_path),
        repository.require_trust_policy().unwrap(),
    )
    .await
    .unwrap();
    let mut row = RepositoryPackage::new(
        repo_id,
        "exact".into(),
        "1.2.3-4".into(),
        VersionScheme::Rpm,
        conary_core::hash::sha256(&bytes),
        bytes.len() as i64,
        format!("http://{address}/exact.rpm"),
    );
    row.architecture = Some("x86_64".into());
    row.source_profile = Some("fedora-44".into());
    let package_id = row.insert(&conn).unwrap();
    let selections = || {
        vec![RepositoryBatchSelection {
            selected: dep_resolution::ResolvedDep {
                package: SatPackage {
                    name: row.name.clone(),
                    version: row.version.clone(),
                    package_release: None,
                    architecture: row.architecture.clone(),
                    version_scheme: row.version_scheme,
                    repo_package_id: Some(package_id),
                    repository_id: Some(repo_id),
                    repository_name: Some(repository.name.clone()),
                    installed_trove_id: None,
                    source: SatSource::Repository,
                },
                required_by: vec!["selected-update".into()],
            },
            install_reason: InstallReason::Dependency,
            selection_reason: "Required by selected update".into(),
            allow_downgrade: false,
            intent: InstallIntent::PackageChange,
        }]
    };
    let before = crate::commands::test_helpers::database_rows(&conn);
    let projection = super::super::preview::PreviewDatabase::new(&conn).unwrap();
    assert!(!keyring_dir(projection.path()).exists());
    let prepared = prepare_repository_batch(
        projection.path(),
        selections(),
        RepositoryBatchMode::Validate,
    )
    .await
    .unwrap();
    assert_eq!(prepared.packages.len(), 1);
    assert_eq!(prepared.packages[0].name, "exact");
    assert_eq!(crate::commands::test_helpers::database_rows(&conn), before);
    assert!(!conary_core::db::paths::objects_dir(&db_path).exists());
    assert!(!keyring_dir(projection.path()).exists());

    // The projected database must not turn absent runtime trust into admission.
    std::fs::remove_dir_all(keyring_dir(&db_path)).unwrap();
    let error = prepare_repository_batch(
        projection.path(),
        selections(),
        RepositoryBatchMode::Validate,
    )
    .await
    .err()
    .expect("missing prepared trust must refuse the dependency");
    assert!(format!("{error:#}").contains("certificate"), "{error:#}");
    assert_eq!(crate::commands::test_helpers::database_rows(&conn), before);
    server.abort();
}
