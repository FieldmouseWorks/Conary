// apps/conary/tests/installed_release_update.rs
#![cfg(feature = "test-hooks")]

// The production update path runs with the explicit test-only mount boundary,
// as in the native mutation fixtures; this proves row and payload selection.

pub mod common;

use conary_core::db::models::{InstallSource, Trove, TroveType};
use conary_core::repository::versioning::VersionScheme;
use std::process::Command;

#[test]
fn update_replaces_the_selected_release_and_preserves_its_sibling() {
    let (temp, db, conn) = common::create_test_db();
    let root = temp.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let (repo_id, key) = common::update_ccs::repository(&conn);
    let ids = ["1", "2"].map(|release| {
        let mut trove = Trove::new_with_source(
            "demo".into(),
            "1.0-1".into(),
            TroveType::Package,
            InstallSource::Repository,
            VersionScheme::Rpm,
        );
        trove.package_release = Some(release.into());
        trove.architecture = Some("x86_64".into());
        trove.source_profile = Some("fedora-44".into());
        trove.installed_from_repository_id = Some(repo_id);
        trove.insert(&conn).unwrap()
    });
    common::update_ccs::candidate(&conn, temp.path(), repo_id, &key, "demo", "x86_64");
    let before = common::database_snapshot(&db);
    for preview in [true, false] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_conary"));
        command
            .args([
                "update",
                "demo",
                "--release",
                "2",
                "--yes",
                "--db-path",
                &db,
                "--root",
            ])
            .arg(&root)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("NO_COLOR", "1")
            .env("CONARY_TEST_SKIP_GENERATION_MOUNT", "1");
        if preview {
            command.arg("--dry-run");
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "preview={preview}: {output:?}");
        if preview {
            assert_eq!(common::database_snapshot(&db), before);
            assert!(!root.join("usr/share/demo").exists());
        }
    }
    let retained = Trove::find_by_id(&conn, ids[0]).unwrap().unwrap();
    assert_eq!(retained.version, "1.0-1");
    assert_eq!(retained.package_release.as_deref(), Some("1"));
    assert!(Trove::find_by_id(&conn, ids[1]).unwrap().is_none());
    let rows = Trove::find_by_name(&conn, "demo").unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows.iter().filter(|row| row.version == "1.0-2").count(), 1);
    assert_eq!(std::fs::read(root.join("usr/share/demo")).unwrap(), b"demo");
}
