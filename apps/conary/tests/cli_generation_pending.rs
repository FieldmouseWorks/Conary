// apps/conary/tests/cli_generation_pending.rs

#![cfg(test)]
//! Publication-debt inspection retains recorded facts without mutating recovery state.

pub mod common;

use conary_core::config_transaction::GenerationConfigTransaction;
use conary_core::db::models::{
    Changeset, GenerationPublication, GenerationPublicationPhase as Phase,
    GenerationPublicationStatus as Status,
};
use std::process::Command;

fn capture(db: &str, tty: bool, no_color: bool) -> String {
    let all_args = vec!["system", "generation", "pending", "--db-path", db];
    let mut command = if tty {
        let arguments = (0..all_args.len())
            .map(|index| format!("\"$CONARY_DEBT_ARG_{index}\""))
            .collect::<Vec<_>>()
            .join(" ");
        let mut command = Command::new("script");
        command
            .args([
                "-qec",
                &format!("exec \"$CONARY_DEBT_EXE\" {arguments}"),
                "/dev/null",
            ])
            .env("CONARY_DEBT_EXE", env!("CARGO_BIN_EXE_conary"));
        for (index, argument) in all_args.iter().enumerate() {
            command.env(format!("CONARY_DEBT_ARG_{index}"), argument);
        }
        command
    } else {
        let mut command = Command::new(env!("CARGO_BIN_EXE_conary"));
        command.args(all_args);
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
    if no_color {
        command.env("NO_COLOR", "1");
    }
    let output = command
        .output()
        .expect("capture requires util-linux script");
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let raw = String::from_utf8(output.stdout).unwrap();
    assert_eq!(raw.contains('\x1b'), tty && !no_color, "{raw:?}");
    assert!(!raw.contains("\x1b[2J"), "{raw:?}");
    console::strip_ansi_codes(&raw).replace("\r\n", "\n")
}

fn pending(
    conn: &rusqlite::Connection,
    db: &str,
    summary: &str,
    changeset: Option<i64>,
) -> GenerationPublication {
    GenerationPublication::create_pending(
        conn,
        changeset,
        None,
        db,
        "/recorded/runtime",
        summary,
        &GenerationConfigTransaction::default(),
    )
    .unwrap()
}

#[test]
fn failed_debt_exposes_exact_cause_and_same_database_retry_without_mutation() {
    for controlled_path in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let name = if controlled_path {
            "selected '\n\x1b[2J.db"
        } else {
            "selected ' $(touch injected).db"
        };
        let path = temp.path().join(name);
        let db = path.to_str().unwrap();
        conary_core::db::init(db).unwrap();
        let conn = conary_core::db::open(db).unwrap();
        let changeset = Changeset::new("Publication fixture".into())
            .insert(&conn)
            .unwrap();
        let debt = GenerationPublication::create_pending(
            &conn,
            Some(changeset),
            None,
            "recorded\n\x1b[2J.db",
            "/runtime\n[ok] forged\x1b[2J",
            "Failure fixture\nsummary",
            &GenerationConfigTransaction::default(),
        )
        .unwrap();
        debt.set_phase(
            &conn,
            Phase::ArtifactReady,
            Status::Running,
            Some(7),
            Some(7),
        )
        .unwrap();
        debt.mark_failed(&conn, "preceding failure").unwrap();
        debt.mark_failed(&conn, "last cause\n[ok] forged\x1b[2J")
            .unwrap();
        let before = common::database_snapshot(db);
        let visible_db = if controlled_path {
            format!("{}/selected '\\n\\u{{1b}}[2J.db", temp.path().display())
        } else {
            db.to_owned()
        };
        let expected = format!(
            concat!(
                "Pending generation publication debt:\n  Database: {visible_db}\n  Records: 1\n",
                "[fail]     Publication debt  1\n  Status: failed\n  Phase: artifact_ready\n",
                "  Changeset: 1\n  Generation: 7\n  State: 7\n",
                "  Recorded database: recorded\\n\\u{{1b}}[2J.db\n",
                "  Runtime root: /runtime\\n[ok] forged\\u{{1b}}[2J\n",
                "  Summary: Failure fixture\\nsummary\n  Retry count: 2\n",
                "  Last error: last cause\\n[ok] forged\\u{{1b}}[2J\n"
            ),
            visible_db = visible_db
        );
        for tty in [false, true] {
            for no_color in [false, true] {
                let text = capture(db, tty, no_color);
                if controlled_path {
                    assert_eq!(
                        text,
                        format!(
                            "{expected}note: To retry publication, run conary system generation publish --yes with --db-path set to this same database path.\n"
                        )
                    );
                    assert!(!text.contains("<PATH>"));
                } else {
                    let (frame, retry) = text
                        .split_once("note: Retry pending publication: ")
                        .unwrap();
                    assert_eq!(frame, expected);
                    assert_eq!(
                        retry.trim(),
                        format!(
                            "conary system generation publish --yes --db-path='{}'",
                            db.replace('\'', "'\"'\"'")
                        )
                    );
                    // Run the printed command with --help to check its real parser and
                    // shell quoting without applying publication or touching the fixture.
                    let output = Command::new("sh")
                        .args(["-c", &format!("{} --help", retry.trim())])
                        .current_dir(temp.path())
                        .env(
                            "PATH",
                            format!(
                                "{}:{}",
                                std::path::Path::new(env!("CARGO_BIN_EXE_conary"))
                                    .parent()
                                    .unwrap()
                                    .display(),
                                std::env::var("PATH").unwrap()
                            ),
                        )
                        .output()
                        .unwrap();
                    assert!(output.status.success(), "{output:?}");
                    assert!(!temp.path().join("injected").exists());
                }
                assert_eq!(common::database_snapshot(db), before);
            }
        }
    }
}

#[test]
fn pending_reader_keeps_typed_filter_and_record_order() {
    let (_temp, db, conn) = common::create_test_db();
    pending(&conn, &db, "first pending", None);
    pending(&conn, &db, "second running", None)
        .set_phase(&conn, Phase::Building, Status::Running, None, None)
        .unwrap();
    pending(&conn, &db, "third failed", None)
        .mark_failed(&conn, "recorded failure")
        .unwrap();
    // Terminal metadata is only a query-exclusion fixture; this test never
    // publishes, activates, or recovers a generation from these records.
    pending(&conn, &db, "excluded complete", None)
        .set_phase(&conn, Phase::DatabaseBackedUp, Status::Complete, None, None)
        .unwrap();
    pending(&conn, &db, "excluded abandoned", None)
        .set_phase(&conn, Phase::PendingBuild, Status::Abandoned, None, None)
        .unwrap();
    let changeset = Changeset::new("Retired debt fixture".into())
        .insert(&conn)
        .unwrap();
    let retired = pending(
        &conn,
        &db,
        "excluded nonrecoverable failure",
        Some(changeset),
    );
    assert_eq!(
        GenerationPublication::abandon_recoverable_for_changeset(&conn, changeset).unwrap(),
        1
    );
    retired
        .mark_failed(&conn, "late failure on retired debt")
        .unwrap();
    let before = common::database_snapshot(&db);
    for tty in [false, true] {
        for no_color in [false, true] {
            let text = capture(&db, tty, no_color);
            assert!(
                text.starts_with(&format!(
                    "Pending generation publication debt:\n  Database: {db}\n  Records: 3\n"
                )),
                "{text}"
            );
            let expected = [
                "[pending]  Publication debt  1\n  Status: pending\n  Phase: pending_build\n  Changeset: -\n  Generation: -\n  State: -\n",
                "[pending]  Publication debt  2\n  Status: running\n  Phase: building\n  Changeset: -\n  Generation: -\n  State: -\n",
                "[fail]     Publication debt  3\n  Status: failed\n  Phase: pending_build\n  Changeset: -\n  Generation: -\n  State: -\n",
            ];
            let positions: Vec<_> = expected
                .iter()
                .map(|frame| {
                    text.find(frame)
                        .unwrap_or_else(|| panic!("missing {frame:?}: {text}"))
                })
                .collect();
            assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
            assert_eq!(text.matches("  Last error: -\n").count(), 2);
            assert!(text.contains("  Last error: recorded failure\n"));
            assert!(!text.contains("excluded"));
            assert!(!text.contains("late failure"));
            assert_eq!(text.matches("note: Retry pending publication: ").count(), 1);
            assert_eq!(common::database_snapshot(&db), before);
        }
    }
}

#[test]
fn empty_debt_result_retains_database_and_stable_empty_line() {
    let (_temp, db, _conn) = common::create_test_db();
    let before = common::database_snapshot(&db);
    for tty in [false, true] {
        for no_color in [false, true] {
            assert_eq!(
                capture(&db, tty, no_color),
                format!(
                    "Pending generation publication debt:\n  Database: {db}\n  Records: 0\nNo pending generation publication debt.\n"
                )
            );
            assert_eq!(common::database_snapshot(&db), before);
        }
    }
}
