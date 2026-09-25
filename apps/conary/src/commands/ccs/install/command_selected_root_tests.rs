// apps/conary/src/commands/ccs/install/command_selected_root_tests.rs

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use conary_core::ccs::{BuildResult, CcsManifest, ComponentData};
use conary_core::hash;
use conary_core::payload::PayloadNodeKind;

use super::command::cmd_ccs_install;
use super::test_support::{
    ccs_regular_file, seed_test_root_layout, stage_test_boot_assets, write_signed_test_package,
};

/// Build the shared fixture package: one regular `sh` payload at `payload_path`
/// plus the `/sbin/init` asset every CCS install fixture needs.
fn signed_sh_package(
    temp_dir: &Path,
    package_name: &str,
    payload_path: &str,
) -> (PathBuf, PathBuf) {
    let package_path = temp_dir.join(format!("{package_name}.ccs"));
    let payload_content = b"#!/bin/sh\nexec true\n".to_vec();
    let payload_hash = hash::sha256(&payload_content);
    let init_content = b"#!/bin/sh\nexec true\n".to_vec();
    let init_hash = hash::sha256(&init_content);
    let files = vec![
        ccs_regular_file(
            payload_path.to_string(),
            payload_hash.clone(),
            payload_content.len() as u64,
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
    let total_size = (payload_content.len() + init_content.len()) as u64;
    let result = BuildResult {
        manifest: CcsManifest::new_minimal(package_name, "1.0.0"),
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
            HashMap::from([(payload_hash, payload_content), (init_hash, init_content)]),
        )
        .unwrap(),
        total_size,
        chunked: false,
        chunk_stats: None,
    };
    let trust_policy_path = write_signed_test_package(&result, &package_path);
    (package_path, trust_policy_path)
}

/// A regular file packaged over host-root symlinks that exist outside the
/// selected root is deployable: the selected root is the only authority. This
/// is also the positive control for the selected-root safety rule exercised in
/// `ccs_install_still_refuses_a_regular_file_over_a_selected_root_symlink`.
#[cfg(unix)]
#[tokio::test]
async fn ccs_install_ignores_host_root_symlinks_outside_the_selected_root() {
    let _mount_guard = crate::commands::composefs_ops::test_mount_skip_guard();
    let temp_dir = tempfile::tempdir().unwrap();
    let install_root = temp_dir.path().join("root");
    let db_path = temp_dir.path().join("conary.db");
    let db_path_str = db_path.to_str().unwrap();

    // The command-level `root` looks like a usr-merged host: /bin -> usr/bin
    // and /usr/bin/sh -> bash. No selected-root layout is seeded, so the real
    // install target has neither symlink.
    std::fs::create_dir_all(install_root.join("usr/bin")).unwrap();
    std::os::unix::fs::symlink("usr/bin", install_root.join("bin")).unwrap();
    std::os::unix::fs::symlink("bash", install_root.join("usr/bin/sh")).unwrap();
    conary_core::db::init(db_path_str).unwrap();
    stage_test_boot_assets(temp_dir.path());

    let (package_path, trust_policy_path) =
        signed_sh_package(temp_dir.path(), "host-root-symlinks", "/bin/sh");

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
    let stored = conary_core::db::models::FileEntry::find_by_path(&conn, "/bin/sh")
        .unwrap()
        .expect("the installed /bin/sh payload must be recorded");
    assert!(
        matches!(&stored.node.source.kind, PayloadNodeKind::Regular { .. }),
        "stored /bin/sh must be a regular file, not a symlink: {stored:?}"
    );
    assert_eq!(
        std::fs::read_link(install_root.join("usr/bin/sh")).unwrap(),
        PathBuf::from("bash"),
        "the unrelated host-root symlink must be untouched"
    );
}

/// The selected-root safety rule still refuses a regular file that would
/// deploy over a symlink inside the selected root. The identical fixture
/// pipeline installs successfully in the sibling test above, so the rejection
/// can only come from this rule.
#[cfg(unix)]
#[tokio::test]
async fn ccs_install_still_refuses_a_regular_file_over_a_selected_root_symlink() {
    let _mount_guard = crate::commands::composefs_ops::test_mount_skip_guard();
    let temp_dir = tempfile::tempdir().unwrap();
    let install_root = temp_dir.path().join("root");
    let db_path = temp_dir.path().join("conary.db");
    let db_path_str = db_path.to_str().unwrap();

    std::fs::create_dir_all(&install_root).unwrap();
    conary_core::db::init(db_path_str).unwrap();
    seed_test_root_layout(
        db_path_str,
        "selected-root-sh",
        &["/usr", "/usr/bin"],
        &[("/usr/bin/sh", "bash")],
    );
    stage_test_boot_assets(temp_dir.path());

    let (package_path, trust_policy_path) =
        signed_sh_package(temp_dir.path(), "selected-root-symlink", "/usr/bin/sh");

    let err = cmd_ccs_install(
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
    .unwrap_err();

    let traversal = err
        .chain()
        .find_map(|cause| cause.downcast_ref::<conary_core::Error>());
    assert!(
        matches!(traversal, Some(conary_core::Error::PathTraversal(_))),
        "expected the selected-root path-safety rule to reject the payload, got: {err:#}"
    );
}
