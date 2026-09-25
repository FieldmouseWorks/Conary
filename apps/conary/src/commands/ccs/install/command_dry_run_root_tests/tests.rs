// apps/conary/src/commands/ccs/install/command_dry_run_root_tests/tests.rs

#![cfg(test)]

use std::collections::HashMap;

use super::command::cmd_ccs_install;
use super::test_support::{ccs_regular_file, seed_test_root_layout};

/// Build a signed package that ships a regular `/usr/bin/sh`. The on-disk host
/// and selected-root baselines differ only in whether that path is a symlink.
fn regular_usr_bin_sh_package(
    temp_dir: &std::path::Path,
    package_name: &str,
) -> (std::path::PathBuf, std::path::PathBuf) {
    use conary_core::ccs::{BuildResult, CcsManifest, ComponentData};

    let package_path = temp_dir.join(format!("{package_name}.ccs"));
    let content = b"#!/bin/sh\nexec true\n".to_vec();
    let content_len = content.len() as u64;
    let hash = conary_core::hash::sha256(&content);
    let files = vec![ccs_regular_file(
        "/usr/bin/sh".to_string(),
        hash.clone(),
        content_len,
        0o100755,
        "runtime".to_string(),
    )];
    let result = BuildResult {
        manifest: CcsManifest::new_minimal(package_name, "1.0.0"),
        components: HashMap::from([(
            "runtime".to_string(),
            ComponentData {
                name: "runtime".to_string(),
                files: files.clone(),
                hash: "test-runtime".to_string(),
                size: content_len,
            },
        )]),
        files: files.clone(),
        payloads: conary_core::ccs::builder::payloads_from_bounded_memory_for_tests(
            &files,
            HashMap::from([(hash, content)]),
        )
        .unwrap(),
        total_size: content_len,
        chunked: false,
        chunk_stats: None,
    };
    let trust_policy_path = super::test_support::write_signed_test_package(&result, &package_path);
    (package_path, trust_policy_path)
}

fn run_dry_run(
    package_path: &std::path::Path,
    trust_policy_path: &std::path::Path,
    db_path: &std::path::Path,
    install_root: &std::path::Path,
) -> anyhow::Result<()> {
    cmd_ccs_install(
        package_path.to_str().unwrap(),
        db_path.to_str().unwrap(),
        install_root.to_str().unwrap(),
        true,
        Some(trust_policy_path.to_string_lossy().into_owned()),
        None,
        crate::commands::SandboxMode::Always,
        true,
        false,
    )
}

#[tokio::test]
async fn ccs_dry_run_ignores_host_symlink_absent_from_selected_root() {
    let temp_dir = tempfile::tempdir().unwrap();
    let install_root = temp_dir.path().join("root");
    let db_path = temp_dir.path().join("conary.db");
    std::fs::create_dir_all(install_root.join("usr/bin")).unwrap();
    std::os::unix::fs::symlink("bash", install_root.join("usr/bin/sh")).unwrap();
    conary_core::db::init(&db_path).unwrap();
    let (package_path, policy_path) =
        regular_usr_bin_sh_package(temp_dir.path(), "preview-host-symlink");

    run_dry_run(&package_path, &policy_path, &db_path, &install_root).unwrap();

    let conn = conary_core::db::open(&db_path).unwrap();
    let troves: i64 = conn
        .query_row("SELECT COUNT(*) FROM troves", [], |row| row.get(0))
        .unwrap();
    assert_eq!(troves, 0, "dry run must not persist a trove");
    assert_eq!(
        std::fs::read_link(install_root.join("usr/bin/sh")).unwrap(),
        std::path::PathBuf::from("bash"),
        "dry run must not replace the live command-root symlink"
    );
    assert!(
        !temp_dir.path().join("selected-root-sessions").exists(),
        "dry run must not create a selected-root session"
    );
    assert!(
        !temp_dir.path().join("objects").exists(),
        "dry run must not create runtime CAS state"
    );
}

#[tokio::test]
async fn ccs_dry_run_refuses_regular_path_beneath_selected_root_symlink() {
    let temp_dir = tempfile::tempdir().unwrap();
    let install_root = temp_dir.path().join("root");
    let db_path = temp_dir.path().join("conary.db");
    std::fs::create_dir_all(&install_root).unwrap();
    conary_core::db::init(&db_path).unwrap();
    seed_test_root_layout(
        db_path.to_str().unwrap(),
        "preview-selected-symlink",
        &["/usr", "/usr/bin"],
        &[("/usr/bin/sh", "bash")],
    );
    let (package_path, policy_path) =
        regular_usr_bin_sh_package(temp_dir.path(), "preview-selected-symlink");

    let error = run_dry_run(&package_path, &policy_path, &db_path, &install_root).unwrap_err();

    let typed = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<conary_core::Error>())
        .unwrap_or_else(|| panic!("expected a typed conary_core error: {error:#}"));
    assert!(
        matches!(typed, conary_core::Error::PathTraversal(_)),
        "expected PathTraversal from the selected-root symlink: {error:#}"
    );
}
