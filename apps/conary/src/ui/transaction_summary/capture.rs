// apps/conary/src/ui/transaction_summary/capture.rs

use super::*;
use crate::commands::test_helpers;
use std::process::Command;

#[test]
fn command_capture_child() {
    let Ok(scenario) = std::env::var("CONARY_TRANSACTION_CAPTURE") else {
        return;
    };
    let _mount = crate::commands::composefs_ops::test_mount_skip_guard();
    let (temp, db_path) = test_helpers::create_test_db();
    test_helpers::seed_test_bootable_runtime(std::path::Path::new(&db_path));
    let conn = conary_core::db::open(&db_path).unwrap();
    let mut trove = Trove::new(
        "summary-fixture".into(),
        "2.0".into(),
        TroveType::Package,
        VersionScheme::Conary,
    );
    trove.architecture = Some("x86_64".into());
    trove.package_release = Some("7".into());
    trove.insert(&conn).unwrap();
    let remove = || {
        crate::commands::cmd_remove(
            "summary-fixture",
            &db_path,
            None,
            None,
            crate::commands::SandboxMode::Always,
            false,
        )
    };
    let rollback = scenario.contains("rollback");
    if rollback {
        remove().unwrap();
    }
    let forward: i64 = conn
        .query_row("SELECT COALESCE(MAX(id), 0) FROM changesets", [], |row| {
            row.get(0)
        })
        .unwrap();
    let _failure = scenario.starts_with("pending").then(|| {
        crate::commands::composefs_ops::test_forced_generation_rebuild_failure_guard(
            "forced summary publication failure",
        )
    });
    if scenario == "failed_remove" {
        conn.execute(
            "UPDATE troves SET pinned = 1 WHERE name = 'summary-fixture'",
            [],
        )
        .unwrap();
    }
    println!("FRAME_BEGIN");
    let result = if rollback {
        crate::commands::cmd_rollback(
            if scenario == "failed_rollback" {
                forward + 999
            } else {
                forward
            },
            &db_path,
        )
    } else {
        remove()
    };
    if scenario.starts_with("failed") {
        assert!(result.is_err());
    } else {
        result.unwrap();
        let installed = Trove::find_by_name(&conn, "summary-fixture").unwrap();
        assert_eq!(installed.len(), usize::from(rollback));
        if rollback {
            assert_eq!(installed[0].package_release.as_deref(), Some("7"));
            let reversed: i64 = conn
                .query_row(
                    "SELECT reversed_by_changeset_id FROM changesets WHERE id = ?1",
                    [forward],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(reversed > forward);
        }
        let pending =
            conary_core::db::models::GenerationPublication::pending_recoverable(&conn).unwrap();
        assert_eq!(pending.is_empty(), !scenario.starts_with("pending"));
    }
    println!("FRAME_END");
    // All disposable state remains alive until after the captured command completes.
    drop(temp);
}

#[test]
fn command_results_in_terminal_pipe_and_no_color() {
    for (tty, no_color) in [(false, false), (false, true), (true, false), (true, true)] {
        for scenario in [
            "remove",
            "rollback",
            "pending_remove",
            "pending_rollback",
            "failed_remove",
            "failed_rollback",
        ] {
            let test = "ui::transaction_summary::tests::capture::command_capture_child";
            let mut command = if tty {
                let mut command = Command::new("script");
                command.args(["-qec", "exec \"$CONARY_TRANSACTION_EXE\" --exact \"$CONARY_TRANSACTION_TEST\" --nocapture", "/dev/null"])
                    .env("CONARY_TRANSACTION_EXE", std::env::current_exe().unwrap())
                    .env("CONARY_TRANSACTION_TEST", test);
                command
            } else {
                let mut command = Command::new(std::env::current_exe().unwrap());
                command.args(["--exact", test, "--nocapture"]);
                command
            };
            command
                .env("CONARY_TRANSACTION_CAPTURE", scenario)
                .env("TERM", "xterm")
                .env_remove("NO_COLOR")
                .env_remove("CLICOLOR_FORCE")
                .env_remove("RUST_LOG");
            if no_color {
                command.env("NO_COLOR", "1");
            }
            let output = command.output().unwrap();
            assert!(output.status.success(), "{scenario}: {output:?}");
            let stdout = String::from_utf8(output.stdout)
                .unwrap()
                .replace("\r\n", "\n");
            let frame = stdout
                .split_once("FRAME_BEGIN\n")
                .unwrap()
                .1
                .split_once("FRAME_END")
                .unwrap()
                .0;
            if !tty || no_color {
                assert!(!frame.contains('\x1b'), "{frame:?}");
            }
            let frame = console::strip_ansi_codes(frame);
            if scenario.starts_with("failed") {
                assert!(!frame.contains("Applied package changes:"), "{frame}");
                assert!(!frame.contains("Generation:"), "{frame}");
                continue;
            }
            assert_eq!(
                frame.matches("Applied package changes:").count(),
                1,
                "{frame}"
            );
            let group = if scenario.contains("rollback") {
                "Restored (1)"
            } else {
                "Removed (1)"
            };
            assert!(frame.contains(group), "{frame}");
            let row = frame
                .lines()
                .find(|line| line.trim_start().starts_with("summary-fixture "))
                .unwrap();
            assert_eq!(
                row.split_whitespace().collect::<Vec<_>>(),
                ["summary-fixture", "2.0", "7", "x86_64"]
            );
            assert!(
                frame.contains("Inspect history: conary system history --db-path '"),
                "{frame}"
            );
            assert!(!frame.contains("Rollback complete"), "{frame}");
            if scenario.contains("rollback") {
                assert!(frame.contains("Reversed changeset: 1"), "{frame}");
                assert!(frame.contains("Changeset: 2"), "{frame}");
                assert!(!frame.contains("Request rollback"), "{frame}");
            } else {
                assert!(
                    frame.contains("conary system state rollback 1 --yes --db-path '"),
                    "{frame}"
                );
            }
            let diagnostic = if tty {
                frame.to_string()
            } else {
                String::from_utf8(output.stderr).unwrap()
            };
            if scenario.starts_with("pending") {
                assert!(frame.contains("Generation: publication pending"), "{frame}");
                assert_eq!(
                    diagnostic
                        .matches("warning: Package mutation committed")
                        .count(),
                    1,
                    "{diagnostic}"
                );
                assert!(
                    diagnostic.contains("publish --yes --db-path '"),
                    "{diagnostic}"
                );
                assert!(!frame.contains(" published"), "{frame}");
            } else {
                assert!(frame.contains(" published"), "{frame}");
                assert!(
                    !diagnostic.contains("warning: Package mutation committed"),
                    "{diagnostic}"
                );
            }
        }
    }
}
