// apps/conary/src/commands/ccs/install/command_strict_installed_tests.rs

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::command::cmd_ccs_install;
use super::test_support::{ccs_regular_file, stage_test_boot_assets};

const PROVIDER_PATH: &str = "/usr/bin/provider-tool";
const CONSUMER_PATH: &str = "/usr/bin/consumer-tool";

struct TestPackage {
    path: PathBuf,
    trust_policy: PathBuf,
}

fn write_package(
    dir: &Path,
    manifest: conary_core::ccs::CcsManifest,
    entries: Vec<(&str, Vec<u8>)>,
) -> TestPackage {
    use conary_core::ccs::{BuildResult, ComponentData};
    use conary_core::hash;

    let package_name = manifest.package.name.clone();
    let mut files = Vec::new();
    let mut payloads = HashMap::new();
    let mut total_size = 0u64;
    for (path, content) in entries {
        let file_hash = hash::sha256(&content);
        let size = content.len() as u64;
        total_size += size;
        files.push(ccs_regular_file(
            path.to_string(),
            file_hash.clone(),
            size,
            0o100755,
            "runtime".to_string(),
        ));
        payloads.insert(file_hash, content);
    }
    let result = BuildResult {
        manifest,
        components: HashMap::from([(
            "runtime".to_string(),
            ComponentData {
                name: "runtime".to_string(),
                files: files.clone(),
                hash: "runtime".to_string(),
                size: total_size,
            },
        )]),
        files: files.clone(),
        payloads: conary_core::ccs::builder::payloads_from_bounded_memory_for_tests(
            &files, payloads,
        )
        .unwrap(),
        total_size,
        chunked: false,
        chunk_stats: None,
    };
    let package_path = dir.join(format!("{package_name}.ccs"));
    let trust_policy = super::test_support::write_signed_test_package(&result, &package_path);
    TestPackage {
        path: package_path,
        trust_policy,
    }
}

fn run_install(
    package: &TestPackage,
    db_path: &str,
    install_root: &str,
    no_deps: bool,
) -> anyhow::Result<()> {
    cmd_ccs_install(
        package.path.to_str().unwrap(),
        db_path,
        install_root,
        false,
        Some(package.trust_policy.to_string_lossy().into_owned()),
        None,
        crate::commands::SandboxMode::Always,
        no_deps,
        false,
    )
}

fn file_predepends(
    path: &str,
) -> conary_core::repository::dependency_model::RepositoryRequirementGroup {
    use conary_core::repository::dependency_model::{
        RepositoryCapabilityKind, RepositoryRequirementClause, RepositoryRequirementGroup,
        RepositoryRequirementKind,
    };

    let mut clause = RepositoryRequirementClause::name_only(path.to_string());
    clause.capability_kind = Some(RepositoryCapabilityKind::File);
    RepositoryRequirementGroup::simple(RepositoryRequirementKind::PreDepends, clause)
}

#[tokio::test]
async fn installed_file_provider_satisfies_consumer_predepends_under_strict_policy() {
    use conary_core::ccs::CcsManifest;

    let _mount_guard = crate::commands::composefs_ops::test_mount_skip_guard();
    let temp_dir = tempfile::tempdir().unwrap();
    let install_root = temp_dir.path().join("root");
    let db_path = temp_dir.path().join("conary.db");
    let db_path_str = db_path.to_str().unwrap();

    std::fs::create_dir_all(&install_root).unwrap();
    conary_core::db::init(db_path_str).unwrap();
    stage_test_boot_assets(temp_dir.path());

    let provider = write_package(
        temp_dir.path(),
        CcsManifest::new_minimal("strict-provider", "1.0.0"),
        vec![
            ("/sbin/init", b"#!/bin/sh\nexec true\n".to_vec()),
            (PROVIDER_PATH, b"provider tool\n".to_vec()),
        ],
    );
    run_install(
        &provider,
        db_path_str,
        install_root.to_str().unwrap(),
        false,
    )
    .unwrap();

    let mut consumer_manifest = CcsManifest::new_minimal("strict-consumer", "1.0.0");
    consumer_manifest
        .requirements
        .push(file_predepends(PROVIDER_PATH));
    let consumer = write_package(
        temp_dir.path(),
        consumer_manifest,
        vec![(CONSUMER_PATH, b"consumer tool\n".to_vec())],
    );
    run_install(
        &consumer,
        db_path_str,
        install_root.to_str().unwrap(),
        false,
    )
    .unwrap();

    let conn = conary_core::db::open(db_path_str).unwrap();
    let consumer_troves =
        conary_core::db::models::Trove::find_by_name(&conn, "strict-consumer").unwrap();
    assert_eq!(consumer_troves.len(), 1, "consumer trove must be installed");
}

#[tokio::test]
async fn consumer_predepends_is_refused_when_no_installed_provider_exists() {
    use conary_core::ccs::CcsManifest;

    let _mount_guard = crate::commands::composefs_ops::test_mount_skip_guard();
    let temp_dir = tempfile::tempdir().unwrap();
    let install_root = temp_dir.path().join("root");
    let db_path = temp_dir.path().join("conary.db");
    let db_path_str = db_path.to_str().unwrap();

    std::fs::create_dir_all(&install_root).unwrap();
    conary_core::db::init(db_path_str).unwrap();
    stage_test_boot_assets(temp_dir.path());

    let mut consumer_manifest = CcsManifest::new_minimal("strict-consumer", "1.0.0");
    consumer_manifest
        .requirements
        .push(file_predepends(PROVIDER_PATH));
    let consumer = write_package(
        temp_dir.path(),
        consumer_manifest,
        vec![(CONSUMER_PATH, b"consumer tool\n".to_vec())],
    );

    let error = run_install(
        &consumer,
        db_path_str,
        install_root.to_str().unwrap(),
        false,
    )
    .unwrap_err();
    let config_error = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<conary_core::Error>());
    assert!(
        matches!(config_error, Some(conary_core::Error::ConfigError(_))),
        "{error:#}"
    );

    let conn = conary_core::db::open(db_path_str).unwrap();
    assert!(
        conary_core::db::models::Trove::find_by_name(&conn, "strict-consumer")
            .unwrap()
            .is_empty(),
        "a refused consumer must not be installed"
    );
}

fn replace_relation(
    name: &str,
) -> conary_core::repository::dependency_model::RepositoryRequirementGroup {
    use conary_core::repository::dependency_model::RepositoryRequirementKind;
    use conary_core::repository::versioning::VersionScheme;

    conary_core::repository::package_relation::parse_native_relation(
        RepositoryRequirementKind::Replace,
        VersionScheme::Conary,
        name,
    )
    .unwrap()
}

fn install_strict_provider(dir: &Path, db_path: &str, install_root: &str) {
    use conary_core::ccs::CcsManifest;

    let provider = write_package(
        dir,
        CcsManifest::new_minimal("strict-provider", "1.0.0"),
        vec![
            ("/sbin/init", b"#!/bin/sh\nexec true\n".to_vec()),
            (PROVIDER_PATH, b"provider tool\n".to_vec()),
        ],
    );
    run_install(&provider, db_path, install_root, false).unwrap();
}

fn write_strict_consumer(dir: &Path, replaces_provider: bool) -> TestPackage {
    use conary_core::ccs::CcsManifest;

    let mut manifest = CcsManifest::new_minimal("strict-consumer", "1.0.0");
    manifest.requirements.push(file_predepends(PROVIDER_PATH));
    if replaces_provider {
        manifest.relations.push(replace_relation("strict-provider"));
    }
    write_package(
        dir,
        manifest,
        vec![(CONSUMER_PATH, b"consumer tool\n".to_vec())],
    )
}

/// Positive control for the replacement case: the identical consumer installs
/// when it does not remove the provider that satisfies its pre-dependency.
#[tokio::test]
async fn consumer_predepends_installs_while_installed_provider_survives() {
    let _mount_guard = crate::commands::composefs_ops::test_mount_skip_guard();
    let temp_dir = tempfile::tempdir().unwrap();
    let install_root = temp_dir.path().join("root");
    let db_path = temp_dir.path().join("conary.db");
    let db_path_str = db_path.to_str().unwrap();
    let install_root_str = install_root.to_str().unwrap();

    std::fs::create_dir_all(&install_root).unwrap();
    conary_core::db::init(db_path_str).unwrap();
    stage_test_boot_assets(temp_dir.path());

    install_strict_provider(temp_dir.path(), db_path_str, install_root_str);
    let consumer = write_strict_consumer(temp_dir.path(), false);
    run_install(&consumer, db_path_str, install_root_str, false).unwrap();

    let conn = conary_core::db::open(db_path_str).unwrap();
    assert_eq!(
        conary_core::db::models::Trove::find_by_name(&conn, "strict-consumer")
            .unwrap()
            .len(),
        1,
        "the consumer must install while its provider survives"
    );
}

/// The consumer's own replacement relation removes the provider that its
/// pre-dependency needs, so the end state is unsatisfiable and the install must
/// be refused before any mutation.
#[tokio::test]
async fn consumer_replacing_its_predepends_provider_is_refused() {
    let _mount_guard = crate::commands::composefs_ops::test_mount_skip_guard();
    let temp_dir = tempfile::tempdir().unwrap();
    let install_root = temp_dir.path().join("root");
    let db_path = temp_dir.path().join("conary.db");
    let db_path_str = db_path.to_str().unwrap();
    let install_root_str = install_root.to_str().unwrap();

    std::fs::create_dir_all(&install_root).unwrap();
    conary_core::db::init(db_path_str).unwrap();
    stage_test_boot_assets(temp_dir.path());

    install_strict_provider(temp_dir.path(), db_path_str, install_root_str);
    let consumer = write_strict_consumer(temp_dir.path(), true);

    let error = run_install(&consumer, db_path_str, install_root_str, false).unwrap_err();
    let config_error = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<conary_core::Error>());
    assert!(
        matches!(config_error, Some(conary_core::Error::ConfigError(_))),
        "{error:#}"
    );

    let conn = conary_core::db::open(db_path_str).unwrap();
    assert_eq!(
        conary_core::db::models::Trove::find_by_name(&conn, "strict-provider")
            .unwrap()
            .len(),
        1,
        "the refused transaction must leave the provider installed"
    );
    assert!(
        conary_core::db::models::Trove::find_by_name(&conn, "strict-consumer")
            .unwrap()
            .is_empty(),
        "a refused consumer must not be installed"
    );
}
