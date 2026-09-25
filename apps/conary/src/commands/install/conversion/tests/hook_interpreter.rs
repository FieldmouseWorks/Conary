// apps/conary/src/commands/install/conversion/tests/hook_interpreter.rs

#![cfg(test)]

use super::*;
use crate::commands::install::ccs_hook_interpreter::{CcsHookInterpreterUnavailable, HookPhase};

/// Write one CCS package whose only hook is `pre_remove` on `/bin/sh`.
///
/// The manifest always declares `/bin/sh` as a `File` provide. `ship_shell`
/// controls whether the payload backs that declaration with an executable
/// node.
fn write_pre_remove_ccs_package(
    temp_dir: &std::path::Path,
    name: &str,
    ship_shell: bool,
) -> std::path::PathBuf {
    let init_content = b"#!/bin/sh\nexec true\n".to_vec();
    let init_hash = hash::sha256(&init_content);
    let mut files = vec![regular_ccs_file(
        "/usr/sbin/init",
        init_hash.clone(),
        init_content.len() as u64,
        0o755,
    )];
    let mut payloads = HashMap::from([(init_hash, init_content)]);
    if ship_shell {
        let shell_content = b"#!/bin/sh\nexit 0\n".to_vec();
        let shell_hash = hash::sha256(&shell_content);
        files.push(regular_ccs_file(
            "/bin/sh",
            shell_hash.clone(),
            shell_content.len() as u64,
            0o755,
        ));
        payloads.insert(shell_hash, shell_content);
    }

    let mut manifest = CcsManifest::new_minimal(name, "1.0.0");
    manifest.provides.files = vec!["/bin/sh".to_string()];
    manifest.hooks.pre_remove = Some(ScriptHook {
        script: "echo removing".to_string(),
        interpreter: "/bin/sh".to_string(),
        reversible: None,
    });
    manifest.components.default = vec!["runtime".to_string()];

    let result = BuildResult {
        manifest,
        components: HashMap::from([(
            "runtime".to_string(),
            ComponentData {
                name: "runtime".to_string(),
                files: files.clone(),
                hash: "runtime".to_string(),
                size: files
                    .iter()
                    .filter_map(|file| file.content.as_ref().map(|content| content.size))
                    .sum(),
            },
        )]),
        files: files.clone(),
        payloads: conary_core::ccs::builder::payloads_from_bounded_memory_for_tests(
            &files, payloads,
        )
        .unwrap(),
        total_size: 0,
        chunked: false,
        chunk_stats: None,
    };
    let signing_key = crate::commands::ccs::load_or_create_local_dev_key().unwrap();
    let package_path = temp_dir.join(format!("{name}.ccs"));
    write_signed_current_ccs_package(&result, &package_path, &signing_key, true).unwrap();
    package_path
}

#[tokio::test]
async fn ccs_install_refuses_missing_pre_remove_interpreter_before_mutation() {
    let _mount_guard = crate::commands::composefs_ops::test_mount_skip_guard();
    let temp_dir = tempfile::tempdir().unwrap();
    let install_root = temp_dir.path().join("root");
    let db_path = temp_dir.path().join("conary.db");
    let db_path_str = db_path.to_str().unwrap();

    std::fs::create_dir_all(&install_root).unwrap();
    conary_core::db::init(db_path_str).unwrap();
    stage_test_boot_assets(temp_dir.path());

    // The pre-remove interpreter must not resolve against the package's own
    // unbacked `/bin/sh` File declaration.
    let package_path =
        write_pre_remove_ccs_package(temp_dir.path(), "pre-remove-hook-interpreter", false);
    let error = install_ccs_artifact(converted_install_options(
        &package_path,
        db_path_str,
        &install_root,
        None,
    ))
    .await
    .unwrap_err();

    let unavailable = error
        .downcast_ref::<CcsHookInterpreterUnavailable>()
        .expect("a missing pre-remove interpreter must be the typed availability refusal");
    assert_eq!(unavailable.package, "pre-remove-hook-interpreter");
    assert_eq!(unavailable.version, "1.0.0");
    assert_eq!(unavailable.phase, HookPhase::PreRemove);
    assert_eq!(unavailable.interpreter, "/bin/sh");

    let conn = conary_core::db::open(db_path_str).unwrap();
    let changesets: i64 = conn
        .query_row("SELECT COUNT(*) FROM changesets", [], |row| row.get(0))
        .unwrap();
    let troves: i64 = conn
        .query_row("SELECT COUNT(*) FROM troves", [], |row| row.get(0))
        .unwrap();
    assert_eq!(changesets, 0, "refusal must not commit a changeset");
    assert_eq!(troves, 0, "refusal must not persist a trove row");
}

#[tokio::test]
async fn ccs_install_admits_pre_remove_interpreter_shipped_in_payload() {
    let _mount_guard = crate::commands::composefs_ops::test_mount_skip_guard();
    let temp_dir = tempfile::tempdir().unwrap();
    let install_root = temp_dir.path().join("root");
    let db_path = temp_dir.path().join("conary.db");
    let db_path_str = db_path.to_str().unwrap();

    std::fs::create_dir_all(&install_root).unwrap();
    conary_core::db::init(db_path_str).unwrap();
    stage_test_boot_assets(temp_dir.path());

    // Positive control on the same package shape: shipping the executable the
    // declaration names clears the availability preflight and persists the
    // removal hook.
    let package_path =
        write_pre_remove_ccs_package(temp_dir.path(), "pre-remove-hook-interpreter", true);
    install_ccs_artifact(converted_install_options(
        &package_path,
        db_path_str,
        &install_root,
        None,
    ))
    .await
    .expect("a payload-backed pre-remove interpreter must clear availability preflight")
    .expect("the admitted install must persist its trove");

    let conn = conary_core::db::open(db_path_str).unwrap();
    let troves: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM troves WHERE name = 'pre-remove-hook-interpreter'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(troves, 1, "the admitted install must persist its trove");
    let script: String = conn
        .query_row(
            "SELECT script FROM installed_ccs_remove_hooks LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(script, "echo removing");
}
