// apps/conary/tests/cli_diagnostics/database_preflight.rs

#![cfg(test)]
//! Common database failures precede package, repository, and system dispatch.

use conary_core::db::schema::{SCHEMA_EPOCH, SCHEMA_VERSION};
use std::path::Path;
use std::process::Command;

const COMMANDS: &[&[&str]] = &[
    &["repo", "sync"],
    &["install", "fixture", "--dry-run"],
    &["system", "history"],
    &["list"],
];

fn capture(args: &[&str], database: &Path, tty: bool, no_color: bool) -> String {
    let mut args = args.to_vec();
    args.extend(["--db-path", database.to_str().unwrap()]);
    let mut command = if tty {
        let arguments = (0..args.len())
            .map(|index| format!("\"$CONARY_PREFLIGHT_ARG_{index}\""))
            .collect::<Vec<_>>()
            .join(" ");
        let mut command = Command::new("script");
        command
            .args([
                "-qec",
                &format!("exec \"$CONARY_PREFLIGHT_EXE\" {arguments}"),
                "/dev/null",
            ])
            .env("CONARY_PREFLIGHT_EXE", env!("CARGO_BIN_EXE_conary"));
        for (index, arg) in args.iter().enumerate() {
            command.env(format!("CONARY_PREFLIGHT_ARG_{index}"), arg);
        }
        command
    } else {
        let mut command = Command::new(env!("CARGO_BIN_EXE_conary"));
        command.args(args);
        command
    };
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("CONARY_TEST_") {
            command.env_remove(name);
        }
    }
    command
        .env("TERM", "xterm")
        .env_remove("RUST_LOG")
        .env_remove("CLICOLOR_FORCE")
        .env_remove("NO_COLOR");
    if no_color {
        command.env("NO_COLOR", "1");
    }
    let output = command
        .output()
        .expect("capture requires util-linux script");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let raw = if tty {
        assert!(output.stderr.is_empty());
        String::from_utf8(output.stdout).unwrap()
    } else {
        assert!(output.stdout.is_empty());
        String::from_utf8(output.stderr).unwrap()
    };
    assert_eq!(raw.contains('\x1b'), tty && !no_color, "{raw:?}");
    assert!(!raw.contains("\x1b[2J"), "{raw:?}");
    console::strip_ansi_codes(&raw).replace("\r\n", "\n")
}

fn visible(value: &str) -> String {
    value
        .chars()
        .flat_map(|ch| {
            if ch.is_control() {
                ch.escape_debug().collect::<Vec<_>>()
            } else {
                vec![ch]
            }
        })
        .collect()
}

fn retained_state(conn: &rusqlite::Connection) -> Vec<(String, String)> {
    let mut statement = conn
        .prepare(
            "SELECT name, sql FROM sqlite_master WHERE type='table'
         UNION ALL SELECT 'version', CAST(version AS TEXT) FROM schema_version
         UNION ALL SELECT 'marker', value FROM fixture_marker
         ORDER BY 1, 2",
        )
        .unwrap();
    let mut rows = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<(String, String)>, _>>()
        .unwrap();
    if rows.iter().any(|(name, _)| name == "schema_identity") {
        rows.extend(
            conn.prepare("SELECT epoch, CAST(revision AS TEXT) FROM schema_identity ORDER BY 1, 2")
                .unwrap()
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
        );
    }
    rows
}

#[test]
fn corrupt_database_failure_names_selected_path_across_first_use_commands() {
    for tty in [false, true] {
        for no_color in [false, true] {
            for args in COMMANDS {
                let temp = tempfile::tempdir().unwrap();
                let db = temp.path().join("selected '$()'\n\x1b[2J.db");
                let bytes = b"not a SQLite database";
                std::fs::write(&db, bytes).unwrap();
                let text = capture(args, &db, tty, no_color);
                assert_eq!(
                    text,
                    format!(
                        "error: Database preflight failed.\n  Database: {}\n  Cause: Database error: file is not a database\n  Cause: file is not a database\n  Cause: Error code 26: file is not a database\nnote: Check the selected database path and reported cause. Preserve existing Conary runtime state before attempting recovery.\n",
                        visible(db.to_str().unwrap())
                    )
                );
                assert_eq!(std::fs::read(&db).unwrap(), bytes);
                assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
            }
        }
    }
}

#[test]
fn obsolete_schema_failure_retains_facts_and_only_offers_rebuild_help() {
    for tty in [false, true] {
        for no_color in [false, true] {
            for args in COMMANDS {
                for epoch in [None, Some("retired\nSupported epoch: forged\x1b[2J")] {
                    let temp = tempfile::tempdir().unwrap();
                    let db = temp.path().join("selected '$()' database.db");
                    let conn = rusqlite::Connection::open(&db).unwrap();
                    conn.execute_batch(
                        "CREATE TABLE schema_version(version INTEGER PRIMARY KEY);
                         INSERT INTO schema_version VALUES(66);
                         CREATE TABLE fixture_marker(value TEXT NOT NULL);
                         INSERT INTO fixture_marker VALUES('preserve retired state');",
                    )
                    .unwrap();
                    if let Some(epoch) = epoch {
                        conn.execute_batch("CREATE TABLE schema_identity(epoch TEXT NOT NULL, revision INTEGER NOT NULL);").unwrap();
                        conn.execute("INSERT INTO schema_identity VALUES(?1, 7)", [epoch])
                            .unwrap();
                    }
                    let before = retained_state(&conn);
                    drop(conn);
                    let observed = epoch.map_or_else(
                        || "retired migration-chain schema version 66".to_string(),
                        |epoch| format!("schema epoch {epoch} revision 7"),
                    );
                    let text = capture(args, &db, tty, no_color);
                    assert_eq!(
                        text,
                        format!(
                            "error: Database requires a schema rebuild.\n  Database: {}\n  Observed schema: {}\n  Supported epoch: {SCHEMA_EPOCH}\n  Supported revision: {SCHEMA_VERSION}\nnote: Preserve existing Conary runtime state unless you have confirmed it is disposable.\nnote: Run: conary system rebuild-db --help\nnote: Any rebuild must select this same database with --db-path and satisfy the command's target and privilege checks.\n",
                            visible(db.to_str().unwrap()),
                            visible(&observed)
                        )
                    );
                    let conn = rusqlite::Connection::open(&db).unwrap();
                    assert_eq!(retained_state(&conn), before);
                    assert_eq!(
                        conary_core::db::schema::inspect(&db).unwrap(),
                        conary_core::db::schema::SchemaCompatibility::RebuildRequired { observed }
                    );
                }
            }
        }
    }
    let help = Command::new(env!("CARGO_BIN_EXE_conary"))
        .args(["system", "rebuild-db", "--help"])
        .output()
        .unwrap();
    assert!(help.status.success(), "{help:?}");
    assert!(help.stderr.is_empty());
    assert!(
        String::from_utf8(help.stdout)
            .unwrap()
            .contains("--db-path")
    );
}
