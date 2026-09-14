// apps/conary/tests/cli_initialization/system_alias.rs

#![cfg(test)]
//! Inspect a retired system database only inside an isolated /var/lib mount.

use super::{MODES, assert_retired, initialize, retire};
use std::path::{Path, PathBuf};
use std::process::Command;

const TEST: &str = "system_alias::system_database_alias_refusal_precedes_retired_schema_inspection";
const PARENT_NAMESPACE: &str = "CONARY_INITIALIZATION_PARENT_MOUNT_NAMESPACE";

#[test]
fn system_database_alias_refusal_precedes_retired_schema_inspection() {
    if let Some(parent_namespace) = std::env::var_os(PARENT_NAMESPACE) {
        assert_ne!(
            std::fs::read_link("/proc/self/ns/mnt").unwrap(),
            PathBuf::from(parent_namespace),
            "the system database fixture requires a separate mount namespace"
        );
        assert!(nix::unistd::Uid::effective().is_root());
        let mount = Command::new("mount")
            .args(["-t", "tmpfs", "-o", "mode=0755", "tmpfs", "/var/lib"])
            .output()
            .unwrap();
        assert!(mount.status.success(), "{mount:?}");

        // /var/lib is now private tmpfs, including when the host has no Conary
        // directory. No host database or parent directory is created or opened.
        let database = Path::new("/var/lib/conary/conary.db");
        retire(database);
        let before = std::fs::read(database).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let alias = temp.path().join("system 'alias'.db");
        std::os::unix::fs::symlink(database, &alias).unwrap();
        for mode in MODES {
            let refusal = initialize(mode, &alias);
            assert_eq!(refusal.code, 1);
            assert_eq!(
                refusal.text,
                format!(
                    "error: the system database must be addressed by its canonical path {}; refusing alias {} so database and runtime state cannot diverge\n",
                    database.display(),
                    alias.display(),
                )
            );
            // Even SQLite's journal configuration must not run before the
            // target is accepted; the rejected alias cannot authorize a read.
            assert_eq!(std::fs::read(database).unwrap(), before);
            assert_retired(database);
        }
        return;
    }

    let mut child = Command::new("unshare");
    if !nix::unistd::Uid::effective().is_root() {
        child.args(["--user", "--map-root-user"]);
    }
    let output = child
        .args(["--mount", "--propagation", "private"])
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", TEST, "--nocapture"])
        .env(
            PARENT_NAMESPACE,
            std::fs::read_link("/proc/self/ns/mnt").unwrap(),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "system alias proof requires usable user/mount namespaces or an isolated privileged test invocation:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("test result: ok. 1 passed; 0 failed; 0 ignored;"),
        "the exact namespace fixture must execute: {output:?}",
    );
}
