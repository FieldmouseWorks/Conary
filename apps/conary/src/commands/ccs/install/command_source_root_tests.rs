// apps/conary/src/commands/ccs/install/command_source_root_tests.rs

//! A real CCS install transaction addressed at one source root leaves every
//! other root, and the default host database, untouched.

use std::collections::{BTreeMap, HashMap};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use conary_core::source_root::{SourceRootName, SourceRootRegistry};

use super::command::cmd_ccs_install;
use super::test_support::{ccs_regular_file, stage_test_boot_assets};

/// Exact per-entry facts: type and mode, size, mtime, inode, and content digest.
type TreeSnapshot = BTreeMap<PathBuf, (u32, u64, i64, i64, u64, Option<String>)>;

fn snapshot_tree(root: &Path) -> TreeSnapshot {
    walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .map(|entry| {
            let entry = entry.unwrap();
            let metadata = entry.path().symlink_metadata().unwrap();
            let digest = metadata
                .file_type()
                .is_file()
                .then(|| conary_core::hash::sha256(&std::fs::read(entry.path()).unwrap()));
            (
                entry.path().strip_prefix(root).unwrap().to_path_buf(),
                (
                    metadata.mode(),
                    metadata.len(),
                    metadata.mtime(),
                    metadata.mtime_nsec(),
                    metadata.ino(),
                    digest,
                ),
            )
        })
        .collect()
}

/// The default host database's exact bytes, or its absence.
fn host_db_state() -> Option<(u64, i64, i64, String)> {
    let path = conary_core::runtime_root::ConaryRuntimeRoot::default()
        .db_path()
        .to_path_buf();
    let metadata = std::fs::metadata(&path).ok()?;
    let digest = std::fs::read(&path)
        .map(|bytes| conary_core::hash::sha256(&bytes))
        .unwrap_or_else(|error| format!("unreadable: {}", error.kind()));
    Some((
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        digest,
    ))
}

fn signed_package(dir: &Path) -> (PathBuf, PathBuf) {
    use conary_core::ccs::{BuildResult, CcsManifest, ComponentData};

    let content = b"from source root".to_vec();
    let file_hash = conary_core::hash::sha256(&content);
    let init_content = b"#!/bin/sh\nexec true\n".to_vec();
    let init_hash = conary_core::hash::sha256(&init_content);
    let total_size = (content.len() + init_content.len()) as u64;
    let files = vec![
        ccs_regular_file(
            "/usr/bin/from-root",
            file_hash.clone(),
            content.len() as u64,
            0o100755,
            "runtime",
        ),
        ccs_regular_file(
            "/sbin/init",
            init_hash.clone(),
            init_content.len() as u64,
            0o100755,
            "runtime",
        ),
    ];
    let result = BuildResult {
        manifest: CcsManifest::new_minimal("from-root", "1.0.0"),
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
    let package_path = dir.join("from-root.ccs");
    let policy_path = super::test_support::write_signed_test_package(&result, &package_path);
    (package_path, policy_path)
}

#[tokio::test]
#[cfg(feature = "test-hooks")]
async fn ccs_install_in_one_source_root_leaves_other_roots_and_host_db_untouched() {
    let _mount_guard = crate::commands::composefs_ops::test_mount_skip_guard();
    let temp = tempfile::tempdir().unwrap();
    let registry = SourceRootRegistry::new(temp.path().join("roots"));
    let root_a = registry
        .create(&SourceRootName::parse("arch").unwrap())
        .unwrap();
    let root_b = registry
        .create(&SourceRootName::parse("fedora-44").unwrap())
        .unwrap();
    stage_test_boot_assets(root_a.root_dir());
    let live_root = temp.path().join("live");
    std::fs::create_dir(&live_root).unwrap();
    let (package_path, policy_path) = signed_package(temp.path());

    let root_b_before = snapshot_tree(root_b.root_dir());
    let host_db_before = host_db_state();
    let root_a_before = snapshot_tree(root_a.root_dir());

    cmd_ccs_install(
        package_path.to_str().unwrap(),
        root_a.db_path().to_str().unwrap(),
        live_root.to_str().unwrap(),
        false,
        Some(policy_path.to_string_lossy().into_owned()),
        None,
        crate::commands::SandboxMode::Always,
        true,
        false,
    )
    .unwrap();

    // Positive control: the transaction ran in root A and published there.
    let conn = conary_core::db::open(root_a.db_path()).unwrap();
    let installed: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM files WHERE path = '/usr/bin/from-root'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(installed, 1);
    drop(conn);
    assert!(std::fs::read_link(root_a.runtime_root().current_link()).is_ok());
    assert_ne!(snapshot_tree(root_a.root_dir()), root_a_before);

    // Root B and the default host database are byte- and mtime-identical.
    assert_eq!(snapshot_tree(root_b.root_dir()), root_b_before);
    assert_eq!(host_db_state(), host_db_before);
    let registry_entries: Vec<_> = std::fs::read_dir(registry.base())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(registry_entries.len(), 2, "{registry_entries:?}");
    assert_eq!(
        registry
            .open(&SourceRootName::parse("fedora-44").unwrap())
            .unwrap(),
        root_b
    );
    assert!(
        !live_root.join("usr/bin/from-root").exists(),
        "source-root installs never deploy into the live root"
    );
}
