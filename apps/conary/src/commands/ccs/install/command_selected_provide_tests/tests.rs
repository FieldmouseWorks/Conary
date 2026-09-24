// apps/conary/src/commands/ccs/install/command_selected_provide_tests/tests.rs

#![cfg(test)]

use std::collections::HashMap;

use super::command::cmd_ccs_install;
use super::test_support::{
    ccs_regular_file, seed_test_init_trove, seed_test_root_layout, stage_test_boot_assets,
    write_signed_test_package,
};
use conary_core::db::models::{ProvideEntry, Trove};
use conary_core::repository::dependency_model::RepositoryCapabilityKind;

#[tokio::test]
async fn ccs_install_keeps_declared_file_provide_for_usrmerged_shipped_path() {
    use conary_core::ccs::{BuildResult, CcsManifest, ComponentData};
    use conary_core::hash;

    let _mount_guard = crate::commands::composefs_ops::test_mount_skip_guard();
    let temp_dir = tempfile::tempdir().unwrap();
    let install_root = temp_dir.path().join("root");
    let package_path = temp_dir.path().join("usrmerge-declared-provide.ccs");
    let db_path = temp_dir.path().join("conary.db");
    let db_path_str = db_path.to_str().unwrap();

    std::fs::create_dir_all(install_root.join("usr/bin")).unwrap();
    std::os::unix::fs::symlink("usr/bin", install_root.join("bin")).unwrap();
    conary_core::db::init(db_path_str).unwrap();
    seed_test_root_layout(
        db_path_str,
        "usrmerge-declared-provide",
        &["/usr", "/usr/bin"],
        &[("/bin", "usr/bin")],
    );
    stage_test_boot_assets(temp_dir.path());

    let content = b"#!/bin/sh\nexec true\n".to_vec();
    let file_hash = hash::sha256(&content);
    let init_content = b"#!/bin/sh\nexec init\n".to_vec();
    let init_hash = hash::sha256(&init_content);
    let total_size = (content.len() + init_content.len()) as u64;
    let files = vec![
        ccs_regular_file(
            "/bin/sh".to_string(),
            file_hash.clone(),
            content.len() as u64,
            0o100755,
            "runtime".to_string(),
        ),
        ccs_regular_file(
            "/sbin/init".to_string(),
            init_hash.clone(),
            init_content.len() as u64,
            0o100755,
            "runtime".to_string(),
        ),
    ];
    let mut manifest = CcsManifest::new_minimal("usrmerge-declared-provide", "1.0.0");
    manifest.provides.files = vec!["/bin/sh".to_string()];

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
            &files,
            HashMap::from([(file_hash, content), (init_hash, init_content)]),
        )
        .unwrap(),
        total_size,
        chunked: false,
        chunk_stats: None,
    };
    let trust_policy_path = write_signed_test_package(&result, &package_path);

    cmd_ccs_install(
        package_path.to_str().unwrap(),
        db_path_str,
        install_root.to_str().unwrap(),
        false,
        Some(trust_policy_path.to_string_lossy().into_owned()),
        None,
        crate::commands::SandboxMode::Always,
        true,
        false,
    )
    .unwrap();

    let conn = conary_core::db::open(db_path_str).unwrap();
    let troves = Trove::find_by_name(&conn, "usrmerge-declared-provide").unwrap();
    assert_eq!(troves.len(), 1, "installed package must be recorded once");
    let trove_id = troves[0].id.expect("installed trove has a database id");
    let provides =
        ProvideEntry::find_by_trove_and_kind(&conn, trove_id, RepositoryCapabilityKind::File)
            .unwrap();
    assert!(
        provides
            .iter()
            .any(|provide| provide.capability == "/bin/sh"),
        "a declared File provide for a shipped usr-merged path must persist: {:?}",
        provides
            .iter()
            .map(|provide| provide.capability.as_str())
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn ccs_component_selection_does_not_persist_uninstalled_declared_file_provide() {
    use conary_core::ccs::{BuildResult, CcsManifest, ComponentData};
    use conary_core::hash;

    let _mount_guard = crate::commands::composefs_ops::test_mount_skip_guard();
    let temp_dir = tempfile::tempdir().unwrap();
    let package_path = temp_dir.path().join("file-provide-selection.ccs");

    let sh_content = b"#!/bin/sh\nexec true\n".to_vec();
    let sh_hash = hash::sha256(&sh_content);
    let readme_content = b"component selection fixture\n".to_vec();
    let readme_hash = hash::sha256(&readme_content);

    let runtime_file = ccs_regular_file(
        "/bin/sh".to_string(),
        sh_hash.clone(),
        sh_content.len() as u64,
        0o100755,
        "runtime".to_string(),
    );
    let docs_file = ccs_regular_file(
        "/usr/share/doc/file-provide-selection/README".to_string(),
        readme_hash.clone(),
        readme_content.len() as u64,
        0o100644,
        "docs".to_string(),
    );
    let files = vec![runtime_file.clone(), docs_file.clone()];

    let mut manifest = CcsManifest::new_minimal("file-provide-selection", "1.0.0");
    manifest.components.default = vec!["runtime".to_string()];
    manifest.provides.files = vec!["/bin/sh".to_string()];

    let result = BuildResult {
        manifest,
        components: HashMap::from([
            (
                "runtime".to_string(),
                ComponentData {
                    name: "runtime".to_string(),
                    files: vec![runtime_file],
                    hash: "runtime".to_string(),
                    size: sh_content.len() as u64,
                },
            ),
            (
                "docs".to_string(),
                ComponentData {
                    name: "docs".to_string(),
                    files: vec![docs_file],
                    hash: "docs".to_string(),
                    size: readme_content.len() as u64,
                },
            ),
        ]),
        files: files.clone(),
        payloads: conary_core::ccs::builder::payloads_from_bounded_memory_for_tests(
            &files,
            HashMap::from([(sh_hash, sh_content), (readme_hash, readme_content)]),
        )
        .unwrap(),
        total_size: 0,
        chunked: false,
        chunk_stats: None,
    };
    let trust_policy_path = write_signed_test_package(&result, &package_path);
    let trust_policy = trust_policy_path.to_string_lossy().into_owned();

    // Negative control: selecting only docs must not persist the declared File
    // provide whose path ships solely in the unselected runtime component.
    let docs_dir = temp_dir.path().join("docs-root");
    let docs_install_root = docs_dir.join("root");
    std::fs::create_dir_all(&docs_install_root).unwrap();
    let docs_db = docs_dir.join("conary.db");
    let docs_db_str = docs_db.to_str().unwrap();
    conary_core::db::init(docs_db_str).unwrap();
    stage_test_boot_assets(&docs_dir);
    seed_test_init_trove(docs_db_str, &docs_dir);

    cmd_ccs_install(
        package_path.to_str().unwrap(),
        docs_db_str,
        docs_install_root.to_str().unwrap(),
        false,
        Some(trust_policy.clone()),
        Some(vec!["docs".to_string()]),
        crate::commands::SandboxMode::Always,
        true,
        false,
    )
    .unwrap();

    let docs_conn = conary_core::db::open(docs_db_str).unwrap();
    let docs_troves = Trove::find_by_name(&docs_conn, "file-provide-selection").unwrap();
    assert_eq!(docs_troves.len(), 1, "fixture trove must be installed once");
    let docs_trove_id = docs_troves[0]
        .id
        .expect("installed trove has a database id");
    let docs_provides = ProvideEntry::find_by_trove_and_kind(
        &docs_conn,
        docs_trove_id,
        RepositoryCapabilityKind::File,
    )
    .unwrap();
    assert!(
        docs_provides
            .iter()
            .all(|provide| provide.capability != "/bin/sh"),
        "installing only the docs component must not persist a File provide for the runtime-only payload path"
    );

    // Positive control through the same package and trust policy: selecting the
    // component that ships the path persists the declared File provide.
    let runtime_dir = temp_dir.path().join("runtime-root");
    let runtime_install_root = runtime_dir.join("root");
    std::fs::create_dir_all(&runtime_install_root).unwrap();
    let runtime_db = runtime_dir.join("conary.db");
    let runtime_db_str = runtime_db.to_str().unwrap();
    conary_core::db::init(runtime_db_str).unwrap();
    stage_test_boot_assets(&runtime_dir);
    seed_test_init_trove(runtime_db_str, &runtime_dir);

    cmd_ccs_install(
        package_path.to_str().unwrap(),
        runtime_db_str,
        runtime_install_root.to_str().unwrap(),
        false,
        Some(trust_policy),
        Some(vec!["runtime".to_string()]),
        crate::commands::SandboxMode::Always,
        true,
        false,
    )
    .unwrap();

    let runtime_conn = conary_core::db::open(runtime_db_str).unwrap();
    let runtime_troves = Trove::find_by_name(&runtime_conn, "file-provide-selection").unwrap();
    assert_eq!(
        runtime_troves.len(),
        1,
        "fixture trove must be installed once"
    );
    let runtime_trove_id = runtime_troves[0]
        .id
        .expect("installed trove has a database id");
    let runtime_provides = ProvideEntry::find_by_trove_and_kind(
        &runtime_conn,
        runtime_trove_id,
        RepositoryCapabilityKind::File,
    )
    .unwrap();
    assert!(
        runtime_provides
            .iter()
            .any(|provide| provide.capability == "/bin/sh"),
        "installing the component that ships the provided path must persist the declared File provide"
    );
}
