// apps/conary/src/commands/ccs/install/command_dry_run_root_tests/tests.rs

#![cfg(test)]

use std::collections::HashMap;

use super::command::cmd_ccs_install;
use super::test_support::{
    TestRootRegularFile, TestRootSpecialNode, ccs_regular_file, seed_test_root_layout,
    seed_test_root_layout_with_regular_files, seed_test_root_layout_with_special_nodes,
};

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

/// Build a signed package that ships a regular `/usr/bin/editor-1` plus an
/// alternatives hook. The hook preflights the target-root
/// `/usr/bin/update-alternatives`, which the baseline must provide as an
/// executable.
fn alternatives_package(
    temp_dir: &std::path::Path,
    package_name: &str,
) -> (std::path::PathBuf, std::path::PathBuf) {
    use conary_core::ccs::manifest::AlternativeHook;
    use conary_core::ccs::{BuildResult, CcsManifest, ComponentData};

    let package_path = temp_dir.join(format!("{package_name}.ccs"));
    let content = b"#!/bin/sh\nexec true\n".to_vec();
    let content_len = content.len() as u64;
    let hash = conary_core::hash::sha256(&content);
    let files = vec![ccs_regular_file(
        "/usr/bin/editor-1".to_string(),
        hash.clone(),
        content_len,
        0o100755,
        "runtime".to_string(),
    )];
    let mut manifest = CcsManifest::new_minimal(package_name, "1.0.0");
    manifest.hooks.alternatives.push(AlternativeHook {
        link: "/usr/bin/editor".to_string(),
        name: "editor".to_string(),
        path: "/usr/bin/editor-1".to_string(),
        priority: 50,
        reversible: None,
    });
    let result = BuildResult {
        manifest,
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

/// Run `test` again inside a user and mount namespace so the target-root
/// executable preflight sees effective root authority.
#[tokio::test]
async fn ccs_dry_run_resolves_alternatives_against_executable_baseline() {
    const TEST_NAME: &str = "commands::ccs::install::command_dry_run_root_tests::ccs_dry_run_resolves_alternatives_against_executable_baseline";
    if !crate::commands::test_helpers::run_exact_test_in_user_mount_namespace(TEST_NAME) {
        return;
    }

    let temp_dir = tempfile::tempdir().unwrap();
    let install_root = temp_dir.path().join("root");
    let db_path = temp_dir.path().join("conary.db");
    std::fs::create_dir_all(&install_root).unwrap();
    conary_core::db::init(&db_path).unwrap();
    seed_test_root_layout_with_regular_files(
        db_path.to_str().unwrap(),
        "preview-alternatives",
        &["/usr", "/usr/bin"],
        &[],
        &[TestRootRegularFile {
            path: "/usr/bin/update-alternatives",
            mode: 0o755,
        }],
    );
    let (package_path, policy_path) = alternatives_package(temp_dir.path(), "preview-alternatives");

    run_dry_run(&package_path, &policy_path, &db_path, &install_root).unwrap();

    let conn = conary_core::db::open(&db_path).unwrap();
    let troves: i64 = conn
        .query_row("SELECT COUNT(*) FROM troves", [], |row| row.get(0))
        .unwrap();
    assert_eq!(troves, 1, "dry run must not persist the previewed package");
}

/// The same alternatives package against a baseline whose target-root
/// `/usr/bin/update-alternatives` is a FIFO. A real apply refuses the FIFO, so
/// the preview must refuse it too instead of materializing an executable
/// regular placeholder.
#[tokio::test]
async fn ccs_dry_run_refuses_fifo_at_lifecycle_program_path() {
    const TEST_NAME: &str = "commands::ccs::install::command_dry_run_root_tests::ccs_dry_run_refuses_fifo_at_lifecycle_program_path";
    if !crate::commands::test_helpers::run_exact_test_in_user_mount_namespace(TEST_NAME) {
        return;
    }

    let temp_dir = tempfile::tempdir().unwrap();
    let install_root = temp_dir.path().join("root");
    let db_path = temp_dir.path().join("conary.db");
    std::fs::create_dir_all(&install_root).unwrap();
    conary_core::db::init(&db_path).unwrap();
    seed_test_root_layout_with_special_nodes(
        db_path.to_str().unwrap(),
        "preview-alternatives-fifo",
        &["/usr", "/usr/bin"],
        &[],
        &[],
        &[TestRootSpecialNode {
            path: "/usr/bin/update-alternatives",
            kind: conary_core::payload::PayloadNodeKind::Fifo,
            mode: libc::S_IFIFO | 0o755,
        }],
    );
    let (package_path, policy_path) =
        alternatives_package(temp_dir.path(), "preview-alternatives-fifo");

    let error = run_dry_run(&package_path, &policy_path, &db_path, &install_root).unwrap_err();

    let typed = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<conary_core::Error>())
        .unwrap_or_else(|| panic!("expected a typed conary_core error: {error:#}"));
    match typed {
        conary_core::Error::ScriptletExecution {
            kind: conary_core::scriptlet::ScriptletFailureKind::ProgramUnavailable,
            ..
        } => {}
        other => panic!(
            "expected ProgramUnavailable for the FIFO lifecycle program, got {other:?}: {error:#}"
        ),
    }
}
