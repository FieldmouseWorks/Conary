// apps/conary/tests/cli_initialization.rs

#![cfg(test)]
//! First-use actions retain the selected database through refusal and recovery.

use conary_core::db::models::Repository;
use conary_core::db::schema::{self, SCHEMA_EPOCH, SCHEMA_VERSION, SchemaCompatibility};
use std::path::Path;
use std::process::Command;

#[derive(Clone, Copy, Debug)]
struct Mode {
    tty: bool,
    no_color: bool,
}

const MODES: [Mode; 4] = [
    Mode {
        tty: false,
        no_color: false,
    },
    Mode {
        tty: false,
        no_color: true,
    },
    Mode {
        tty: true,
        no_color: false,
    },
    Mode {
        tty: true,
        no_color: true,
    },
];

struct Capture {
    code: i32,
    text: String,
    raw: String,
}

fn run(mode: Mode, args: &[&str]) -> Capture {
    run_at(mode, args, None)
}

fn run_at(mode: Mode, args: &[&str], directory: Option<&Path>) -> Capture {
    let mut command = if mode.tty {
        let arguments = (0..args.len())
            .map(|index| format!("\"$CONARY_INITIALIZATION_ARG_{index}\""))
            .collect::<Vec<_>>()
            .join(" ");
        let mut command = Command::new("script");
        command.args([
            "-qec",
            &format!("exec \"$CONARY_INITIALIZATION_EXE\" {arguments}"),
            "/dev/null",
        ]);
        command.env("CONARY_INITIALIZATION_EXE", env!("CARGO_BIN_EXE_conary"));
        for (index, argument) in args.iter().enumerate() {
            command.env(format!("CONARY_INITIALIZATION_ARG_{index}"), argument);
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
        .env_remove("NO_COLOR")
        .env_remove("CLICOLOR_FORCE");
    if mode.no_color {
        command.env("NO_COLOR", "1");
    }
    if let Some(directory) = directory {
        command.current_dir(directory);
    }
    let output = command
        .output()
        .expect("PTY capture requires util-linux script");
    if mode.tty || !output.status.success() {
        let empty_stream = if mode.tty {
            &output.stderr
        } else {
            &output.stdout
        };
        assert!(empty_stream.is_empty(), "{mode:?}: {output:?}");
    }
    let raw = format!(
        "{}{}",
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap()
    );
    assert_eq!(
        raw.contains('\x1b'),
        mode.tty && !mode.no_color,
        "{mode:?}: {raw:?}"
    );
    Capture {
        code: output.status.code().unwrap(),
        text: console::strip_ansi_codes(&raw).replace("\r\n", "\n"),
        raw,
    }
}

fn initialize(mode: Mode, db: &Path) -> Capture {
    run(mode, &["system", "init", "--db-path", db.to_str().unwrap()])
}

/// Ask the shell to decode the printed command without performing its action.
fn printed_arguments(capture: &Capture) -> Vec<String> {
    let action = capture
        .text
        .lines()
        .find_map(|line| line.strip_prefix("note: Run: "))
        .expect("actionable command");
    assert!(action.starts_with("conary "), "{action}");
    let output = Command::new("sh")
        .args([
            "-c",
            &format!("conary() {{ printf '%s\\0' \"$@\"; }}; {action}"),
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty());
    output
        .stdout
        .strip_suffix(&[0])
        .expect("shell argument terminator")
        .split(|byte| *byte == 0)
        .map(|arg| String::from_utf8(arg.to_vec()).unwrap())
        .collect()
}

fn retire(db: &Path) {
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute_batch(
        "CREATE TABLE schema_version (version INTEGER PRIMARY KEY);
         INSERT INTO schema_version VALUES (66);
         CREATE TABLE fixture_marker (value TEXT NOT NULL);
         INSERT INTO fixture_marker VALUES ('preserve retired state');",
    )
    .unwrap();
}

fn assert_retired(db: &Path) {
    assert_eq!(
        schema::inspect(db).unwrap(),
        SchemaCompatibility::RebuildRequired {
            observed: "retired migration-chain schema version 66".to_string(),
        }
    );
    let conn = rusqlite::Connection::open(db).unwrap();
    let marker: String = conn
        .query_row("SELECT value FROM fixture_marker", [], |row| row.get(0))
        .unwrap();
    assert_eq!(marker, "preserve retired state");
    let tables: String = conn.query_row(
        "SELECT group_concat(name, ',') FROM (SELECT name FROM sqlite_master WHERE type='table' ORDER BY name)",
        [], |row| row.get(0),
    ).unwrap();
    assert_eq!(tables, "fixture_marker,schema_version");
}

fn assert_current(db: &Path) {
    assert_eq!(schema::inspect(db).unwrap(), SchemaCompatibility::Current);
    let conn = conary_core::db::open(db).unwrap();
    conary_core::ccs::HostCapabilityInventory::load_required(&conn).unwrap();
    assert!(!Repository::list_enabled(&conn).unwrap().is_empty());
}

#[test]
fn initialization_keeps_the_selected_database_in_the_executable_sync_action() {
    for mode in MODES {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("state 'quoted'/conary.db");
        let capture = initialize(mode, &db);
        assert_eq!(capture.code, 0, "{}", capture.text);
        assert!(capture.text.starts_with(&format!(
            "Initialized Conary database\n  Database: {}\n",
            db.display()
        )));
        assert_current(&db);
        let arguments = printed_arguments(&capture);
        assert_eq!(
            arguments,
            [
                "repo".to_string(),
                "sync".to_string(),
                format!("--db-path={}", db.display())
            ]
        );

        // Keep the actual printed follow-up offline while proving its DB binding.
        let conn = conary_core::db::open(&db).unwrap();
        conn.execute("UPDATE repositories SET enabled = 0", [])
            .unwrap();
        drop(conn);
        let sync = run(
            mode,
            &arguments.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        assert_eq!(sync.code, 0, "{}", sync.text);
        assert!(
            sync.text.contains("No enabled repositories to sync."),
            "{}",
            sync.text
        );
    }
}

#[test]
fn unusable_parent_refusal_retains_location_and_preserves_the_file() {
    for mode in MODES {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("not a directory");
        std::fs::write(&parent, b"keep this file").unwrap();
        let db = parent.join("conary.db");
        let capture = initialize(mode, &db);
        assert_eq!(capture.code, 1);
        assert!(capture.text.starts_with(&format!(
            "error: Database initialization failed.\n  Database: {}\n  Database parent: {}\n  Runtime root: {}\n  Cause: ",
            db.display(), parent.display(), parent.display(),
        )), "{}", capture.text);
        assert!(
            capture
                .text
                .contains("note: Check that the database parent is a writable directory")
        );
        assert!(
            capture
                .text
                .contains("note: Preserve existing Conary runtime state")
        );
        assert_eq!(std::fs::read(&parent).unwrap(), b"keep this file");
        assert!(!db.exists());
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
    }
}

#[test]
fn relative_database_refusal_names_the_current_directory() {
    for mode in MODES {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("conary.db");
        std::fs::create_dir(&directory).unwrap();
        let refusal = run_at(
            mode,
            &["system", "init", "--db-path", "conary.db"],
            Some(temp.path()),
        );
        assert_eq!(refusal.code, 1);
        assert!(refusal.text.starts_with(
            "error: Database initialization failed.\n  Database: conary.db\n  Database parent: .\n  Runtime root: .\n  Cause: "
        ), "{}", refusal.text);
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
        assert_eq!(std::fs::read_dir(directory).unwrap().count(), 0);
    }
}

#[test]
fn retired_preflight_refusal_retains_facts_and_prints_a_working_rebuild_action() {
    for mode in MODES {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("retired 'quoted' state");
        let db = parent.join("conary.db");
        retire(&db);
        let capture = initialize(mode, &db);
        assert_eq!(capture.code, 1);
        assert!(capture.text.starts_with(&format!(
            "error: Database initialization requires a schema rebuild.\n  Database: {}\n  Database parent: {}\n  Runtime root: {}\n  Observed schema: retired migration-chain schema version 66\n  Supported epoch: {SCHEMA_EPOCH}\n  Supported revision: {SCHEMA_VERSION}\n",
            db.display(), parent.display(), parent.display(),
        )), "{}", capture.text);
        assert!(
            capture
                .text
                .contains("Use it only when this state is disposable.")
        );
        assert_retired(&db);
        assert!(!parent.join("backups").exists());
        let arguments = printed_arguments(&capture);
        assert_eq!(
            arguments,
            [
                "system".to_string(),
                "rebuild-db".to_string(),
                "--discard-state".to_string(),
                "--yes".to_string(),
                format!("--db-path={}", db.display())
            ]
        );
        let rebuild = run(
            mode,
            &arguments.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        assert_eq!(rebuild.code, 0, "{}", rebuild.text);
        assert!(rebuild.text.starts_with(&format!(
            "Rebuilt Conary database with the current schema\n  Database: {}\n",
            db.display()
        )));
        assert_current(&db);
        let snapshots = std::fs::read_dir(parent.join("backups"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(snapshots.len(), 1);
        assert_retired(&snapshots[0]);
        assert!(
            rebuild
                .text
                .contains(&format!("  Retired snapshot: {}\n", snapshots[0].display()))
        );
    }
}

#[test]
fn control_character_paths_stay_data_in_initialization_and_recovery() {
    for mode in MODES {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("state\n\x1b[31mrow");
        let db = parent.join("conary.db");
        let shown_parent = format!("{}/state\\n\\u{{1b}}[31mrow", temp.path().display());
        let capture = initialize(mode, &db);
        assert_eq!(capture.code, 0, "{}", capture.text);
        assert!(
            capture
                .text
                .contains(&format!("  Database: {shown_parent}/conary.db\n"))
        );
        assert!(!capture.raw.contains("state\n"));
        assert!(!capture.raw.contains("\x1b[31mrow"));
        assert!(capture.text.contains(
            "note: Run: conary repo sync --db-path <PATH> (use the same database path)\n"
        ));
        assert_current(&db);

        let retired = parent.join("retired.db");
        retire(&retired);
        let refusal = initialize(mode, &retired);
        assert_eq!(refusal.code, 1);
        assert!(
            refusal
                .text
                .contains(&format!("  Database: {shown_parent}/retired.db\n"))
        );
        assert!(refusal.text.contains("note: Run: conary system rebuild-db --discard-state --yes --db-path <PATH> (use the same database path)\n"));
        assert_retired(&retired);
        // The placeholder is guidance, never a shell command to execute.
        let rebuild = run(
            mode,
            &[
                "system",
                "rebuild-db",
                "--discard-state",
                "--yes",
                "--db-path",
                retired.to_str().unwrap(),
            ],
        );
        assert_eq!(rebuild.code, 0, "{}", rebuild.text);
        assert!(
            rebuild
                .text
                .contains(&format!("  Database: {shown_parent}/retired.db\n"))
        );
        assert!(
            rebuild
                .text
                .contains(&format!("  Retired snapshot: {shown_parent}/backups/"))
        );
        assert!(!rebuild.raw.contains("state\n"));
        assert!(!rebuild.raw.contains("\x1b[31mrow"));
        assert_current(&retired);
    }
}
