// apps/conary/tests/cli_path_query.rs

#![cfg(test)]
//! File ownership queries must propagate failed owner observations.

pub mod common;

use conary_core::db::{
    self,
    models::{FileEntry, Trove},
};
use std::process::{Command, Output};

fn query(db_path: &str, path: &str, info: bool) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_conary"));
    command
        .args(["list", "--path", path, "--db-path", db_path])
        .env("NO_COLOR", "1");
    if info {
        command.arg("--info");
    }
    command.output().unwrap()
}

fn failed_owner_observation(path: &str, info: bool, corrupt_grammar: bool) {
    let (_temp, db_path) = common::setup_command_test_db();
    let conn = db::open(&db_path).unwrap();
    conn.pragma_update(None, "ignore_check_constraints", true)
        .unwrap();
    conn.execute(
        if corrupt_grammar {
            "UPDATE troves SET version_scheme = 'unknown' WHERE name = 'nginx'"
        } else {
            "UPDATE troves SET type = 'bogus' WHERE name = 'nginx'"
        },
        [],
    )
    .unwrap();
    conn.pragma_update(None, "ignore_check_constraints", false)
        .unwrap();
    // Establish that valid file evidence reaches a failed owner read.
    let file = FileEntry::find_by_path(&conn, "/usr/sbin/nginx")
        .unwrap()
        .unwrap();
    let owner_error = Trove::find_by_id(&conn, file.trove_id)
        .unwrap_err()
        .to_string();
    drop(conn);
    let before = common::database_snapshot(&db_path);
    let output = query(&db_path, path, info);
    assert!(
        !output.status.success(),
        "owner-read error was suppressed: {owner_error}; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&owner_error),
        "original error lost: {owner_error}; stderr: {stderr}"
    );
    assert_eq!(common::database_snapshot(&db_path), before);
}

#[test]
fn exact_path_preserves_invalid_owner_type() {
    failed_owner_observation("/usr/sbin/nginx", false, false);
}

#[test]
fn pattern_path_preserves_invalid_owner_type() {
    failed_owner_observation("/usr/*", false, false);
}

#[test]
fn exact_info_preserves_invalid_owner_grammar() {
    failed_owner_observation("/usr/sbin/nginx", true, true);
}

#[test]
fn pattern_path_preserves_invalid_owner_grammar() {
    failed_owner_observation("/usr/sbin/ngi*", false, true);
}

#[test]
fn valid_absence_exact_pattern_and_info_queries_remain_read_only() {
    let (_temp, db_path) = common::setup_command_test_db();
    let before = common::database_snapshot(&db_path);
    for (path, info, expected) in [
        (
            "/no-such-fixture-file",
            false,
            vec!["No package owns file matching '/no-such-fixture-file'"],
        ),
        (
            "/usr/sbin/nginx",
            false,
            vec!["nginx 1.24.0 provides:", "/usr/sbin/nginx"],
        ),
        ("/usr/*", false, vec!["nginx 1.24.0:", "openssl 3.0.0:"]),
        (
            "/usr/sbin/nginx",
            true,
            vec![
                "Installed package:",
                "  Name: nginx\n",
                "  Version: 1.24.0\n",
            ],
        ),
    ] {
        let output = query(&db_path, path, info);
        assert!(output.status.success(), "{output:?}");
        let text = String::from_utf8_lossy(&output.stdout);
        for expected in expected {
            assert!(text.contains(expected), "missing {expected:?}: {text}");
        }
        assert_eq!(common::database_snapshot(&db_path), before);
    }
}
