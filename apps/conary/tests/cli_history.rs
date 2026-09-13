// apps/conary/tests/cli_history.rs

#![cfg(test)]
//! Real history commands preserve recorded evidence across terminal modes without mutation.

use conary_core::db::{
    self,
    models::{
        Changeset, ChangesetStatus, GenerationPublication, LifecycleEvent, NewLifecycleEvent,
    },
};
use conary_core::scriptlet::{
    EffectiveSandbox, SandboxMode, ScriptletFailureKind, ScriptletFailureOutcome,
};
use rusqlite::{Connection, params, types::Value};
use std::path::PathBuf;
use std::process::{Command, Output};

struct Fixture {
    temp: tempfile::TempDir,
    db: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("operator's history.db");
        db::init(db.to_str().unwrap()).unwrap();
        Self { temp, db }
    }

    fn seed(&self) -> (i64, i64, i64) {
        let conn = db::open(self.db.to_str().unwrap()).unwrap();
        let mut original = Changeset::new("Install fixture\nforged\u{1b}[31m".into());
        let original_id = original.insert(&conn).unwrap();
        LifecycleEvent::append_batch(
            &conn,
            original_id,
            &[NewLifecycleEvent {
                source_package: "lifecycle-fixture".into(),
                source_version: "1.0.0".into(),
                source_entry: "rpm:%post".into(),
                failure: ScriptletFailureOutcome {
                    phase: "post-install".into(),
                    failure_kind: ScriptletFailureKind::ScriptExited,
                    requested_sandbox_mode: SandboxMode::Always,
                    effective_sandbox: EffectiveSandbox::TargetRoot,
                    message: "script returned 42\nnot a new record".into(),
                },
            }],
        )
        .unwrap();
        original
            .update_status(&conn, ChangesetStatus::Applied)
            .unwrap();
        let publication = GenerationPublication::create_pending(
            &conn,
            Some(original_id),
            None,
            self.db.to_str().unwrap(),
            self.temp.path().to_str().unwrap(),
            "fixture publication",
            &Default::default(),
        )
        .unwrap();
        publication
            .mark_failed(&conn, "recorded publication failure")
            .unwrap();
        let metadata = serde_json::json!({
            "schema": "conary.changeset.metadata.v7",
            "removed_troves": [],
            "deferred_follow_up": [
                {"kind":"generation_publication", "status":"pending", "message":"publication pending", "retry_command":"stale database command"},
                {"kind":"other", "status":"pending", "message":"recorded guidance", "retry_command":"recorded follow-up command"}
            ],
            "adoption_warnings": []
        });
        let mut rollback = Changeset::new_rollback("Rollback fixture".into(), original_id);
        let rollback_id = rollback.insert(&conn).unwrap();
        rollback
            .update_status(&conn, ChangesetStatus::Applied)
            .unwrap();
        original
            .update_status(&conn, ChangesetStatus::RolledBack)
            .unwrap();
        let pending_id = Changeset::new("Pending fixture".into())
            .insert(&conn)
            .unwrap();
        conn.execute(
            "UPDATE changesets SET created_at = '2026-01-01 00:00:00', applied_at = '2026-01-01 00:01:00', rolled_back_at = '2026-01-02 00:01:00', reversed_by_changeset_id = ?1, metadata = ?2 WHERE id = ?3",
            params![rollback_id, metadata.to_string(), original_id],
        ).unwrap();
        conn.execute(
            "UPDATE changesets SET created_at = '2026-01-02 00:00:00', applied_at = '2026-01-02 00:01:00' WHERE id = ?1",
            [rollback_id],
        ).unwrap();
        conn.execute(
            "UPDATE changesets SET created_at = '2026-01-03 00:00:00' WHERE id = ?1",
            [pending_id],
        )
        .unwrap();
        (original_id, rollback_id, pending_id)
    }

    fn capture(&self, tty: bool, no_color: bool) -> Output {
        let mut command = if tty {
            let mut command = Command::new("script");
            command.args([
                "-qec",
                "exec \"$CONARY_HISTORY_EXE\" system history --db-path \"$CONARY_HISTORY_DB\"",
                "/dev/null",
            ]);
            command.env("CONARY_HISTORY_EXE", env!("CARGO_BIN_EXE_conary"));
            command.env("CONARY_HISTORY_DB", &self.db);
            command
        } else {
            let mut command = Command::new(env!("CARGO_BIN_EXE_conary"));
            command
                .args(["system", "history", "--db-path"])
                .arg(&self.db);
            command
        };
        command
            .env("XDG_CONFIG_HOME", self.temp.path().join("config"))
            .env("XDG_DATA_HOME", self.temp.path().join("data"))
            .env("TERM", "xterm")
            .env_remove("NO_COLOR")
            .env_remove("CLICOLOR_FORCE");
        if no_color {
            command.env("NO_COLOR", "1");
        }
        command
            .output()
            .expect("capture history (requires util-linux script)")
    }

    fn rows(&self) -> Vec<Vec<Vec<Value>>> {
        let conn = db::open(self.db.to_str().unwrap()).unwrap();
        ["changesets", "generation_publications", "lifecycle_events"]
            .map(|table| table_rows(&conn, table))
            .to_vec()
    }
}

fn table_rows(conn: &Connection, table: &str) -> Vec<Vec<Value>> {
    let mut statement = conn
        .prepare(&format!("SELECT * FROM {table} ORDER BY id"))
        .unwrap();
    let columns = statement.column_count();
    statement
        .query_map([], |row| {
            (0..columns)
                .map(|column| row.get(column))
                .collect::<rusqlite::Result<Vec<Value>>>()
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

fn plain(output: &Output, tty: bool, no_color: bool, styled: bool) -> String {
    let raw = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .replace("\r\n", "\n");
    if tty && !no_color && styled {
        assert!(
            raw.contains('\x1b'),
            "terminal styling was not exercised: {raw:?}"
        );
    } else {
        assert!(!raw.contains('\x1b'), "unexpected terminal escape: {raw:?}");
    }
    console::strip_ansi_codes(&raw).into_owned()
}

#[test]
fn empty_history_is_identical_in_terminal_pipe_and_no_color() {
    let fixture = Fixture::new();
    let before = fixture.rows();
    for (tty, no_color) in [(false, false), (false, true), (true, false), (true, true)] {
        let output = fixture.capture(tty, no_color);
        assert!(output.status.success(), "{output:?}");
        assert_eq!(
            plain(&output, tty, no_color, false),
            "No changeset history.\n"
        );
        assert_eq!(fixture.rows(), before);
    }
}

#[test]
fn recorded_history_and_recovery_survive_all_output_modes_without_mutation() {
    let fixture = Fixture::new();
    let (original, rollback, pending) = fixture.seed();
    let before = fixture.rows();
    let mut reference = None;
    for (tty, no_color) in [(false, false), (false, true), (true, false), (true, true)] {
        let output = fixture.capture(tty, no_color);
        assert!(output.status.success(), "{output:?}");
        let frame = plain(&output, tty, no_color, true);
        if let Some(reference) = &reference {
            assert_eq!(&frame, reference);
        }
        let pending_frame = frame
            .split_once(&format!("Changeset {pending}:\n"))
            .unwrap()
            .1
            .split("Changeset ")
            .next()
            .unwrap();
        assert!(pending_frame.contains("  Status: pending\n"));
        assert!(
            !pending_frame.contains("  Applied:")
                && !pending_frame.contains("Generation publication:")
        );
        let rollback_frame = frame
            .split_once(&format!("Changeset {rollback}:\n"))
            .unwrap()
            .1
            .split("Changeset ")
            .next()
            .unwrap();
        assert!(rollback_frame.contains("  Kind: rollback\n  Status: applied\n"));
        assert!(rollback_frame.contains(&format!("  Reverses changeset: {original}\n")));
        assert!(!rollback_frame.contains("Generation publication:"));
        for expected in [
            "  Description: Install fixture\\nforged\\u{1b}[31m\n".to_owned(),
            "  Status: rolled_back\n".into(),
            "  Rolled back: 2026-01-02 00:01:00\n".into(),
            format!("  Reversed by changeset: {rollback}\n"),
            "  Generation publication: failed\nDeferred work (2):\n".into(),
            "  Kind: generation_publication\n  Status: pending\n".into(),
            "[warn]".into(),
            "Continued lifecycle failure\n  Package: lifecycle-fixture\n  Version: 1.0.0\n  Entry: rpm:%post\n  Failure: ScriptExited\n  Phase: post-install\n  Requested sandbox: always\n  Effective sandbox: target-root\n  Reason: script returned 42\\nnot a new record\n".into(),
            "  Total changesets: 3\n".into(),
        ] { assert!(frame.contains(&expected), "missing {expected:?}: {frame}"); }
        let scoped_retry = format!(
            "note: Retry: conary system generation publish --yes --db-path='{}'\n",
            fixture.db.to_str().unwrap().replace('\'', "'\"'\"'")
        );
        assert_eq!(frame.matches(&scoped_retry).count(), 1, "{frame}");
        assert_eq!(
            frame
                .matches("note: Retry: recorded follow-up command\n")
                .count(),
            1
        );
        assert_eq!(frame.matches("note: Retry:").count(), 2);
        assert!(!frame.contains("stale database command") && !frame.contains("Request rollback:"));
        assert!(
            frame.find(&format!("Changeset {pending}:"))
                < frame.find(&format!("Changeset {rollback}:"))
        );
        assert!(
            frame.find(&format!("Changeset {rollback}:"))
                < frame.find(&format!("Changeset {original}:"))
        );
        assert_eq!(fixture.rows(), before);
        if let Ok(directory) = std::env::var("CONARY_HISTORY_CAPTURE_DIR") {
            std::fs::create_dir_all(&directory).unwrap();
            let sanitized = frame.replace(fixture.temp.path().to_str().unwrap(), "<fixture>");
            std::fs::write(
                PathBuf::from(directory).join(format!("history-tty-{tty}-no-color-{no_color}.txt")),
                sanitized,
            )
            .unwrap();
        }
        reference = Some(frame);
    }
}

#[test]
fn obsolete_metadata_remains_a_failure_without_mutation() {
    let fixture = Fixture::new();
    let conn = db::open(fixture.db.to_str().unwrap()).unwrap();
    let id = Changeset::new("obsolete fixture".into())
        .insert(&conn)
        .unwrap();
    conn.execute(
        "UPDATE changesets SET metadata = ?1 WHERE id = ?2",
        params![r#"{"schema":"conary.changeset.metadata.v5"}"#, id],
    )
    .unwrap();
    drop(conn);
    let before = fixture.rows();
    let output = fixture.capture(false, false);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Unsupported changeset metadata schema"),
        "{stderr}"
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("Total changesets:"));
    assert_eq!(fixture.rows(), before);
}
