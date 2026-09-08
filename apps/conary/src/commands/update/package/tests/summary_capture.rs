// apps/conary/src/commands/update/package/tests/summary_capture.rs

use super::*;
use std::process::Command;

#[path = "summary_capture/fixtures.rs"]
mod fixtures;
use fixtures::add_candidate;

#[tokio::test]
async fn update_summary_capture_child() {
    let Ok(scenario) = std::env::var("CONARY_UPDATE_SUMMARY_CAPTURE") else {
        return;
    };
    let _mount = crate::commands::composefs_ops::test_mount_skip_guard();
    let (temp, db_path) = create_test_db();
    seed_test_bootable_runtime(Path::new(&db_path));
    let conn = conary_core::db::open(&db_path).unwrap();
    add_candidate(
        &conn,
        temp.path(),
        "a-summary-update",
        false,
        scenario.starts_with("relation_"),
        scenario
            .starts_with("sequence_")
            .then_some("z-summary-update"),
    );
    if scenario == "mixed" {
        add_candidate(&conn, temp.path(), "z-summary-failed", true, false, None);
    }
    if scenario.starts_with("sequence_") {
        add_candidate(&conn, temp.path(), "z-summary-update", false, false, None);
    }
    if scenario == "noop" {
        conn.execute(
            "UPDATE troves SET pinned = 1 WHERE name = 'a-summary-update'",
            [],
        )
        .unwrap();
    }
    let _failure = (scenario == "pending").then(|| {
        crate::commands::composefs_ops::test_forced_generation_rebuild_failure_guard(
            "forced update summary publication failure",
        )
    });
    let before = crate::commands::test_helpers::database_rows(&conn);
    println!("FRAME_BEGIN");
    let result = update_packages(
        None,
        &db_path,
        temp.path().to_str().unwrap(),
        false,
        matches!(
            scenario.as_str(),
            "preview" | "relation_preview" | "sequence_preview"
        ),
        SandboxMode::Always,
        None,
        true,
        None,
        None,
        true,
    )
    .await;
    println!("FRAME_END");
    if scenario == "mixed" {
        assert!(
            result.is_err(),
            "mixed lifecycle failure unexpectedly succeeded"
        );
    } else {
        assert_eq!(
            result.unwrap(),
            match scenario.as_str() {
                "sequence_preview" =>
                    crate::commands::update::outcome::UpdateOutcome::Planned { packages: 2 },
                "sequence_apply" =>
                    crate::commands::update::outcome::UpdateOutcome::Applied { packages: 2 },
                "preview" | "relation_preview" =>
                    crate::commands::update::outcome::UpdateOutcome::Planned { packages: 1 },
                "noop" => crate::commands::update::outcome::UpdateOutcome::NoChanges,
                _ => crate::commands::update::outcome::UpdateOutcome::Applied { packages: 1 },
            }
        );
    }
    if matches!(
        scenario.as_str(),
        "preview" | "relation_preview" | "sequence_preview" | "noop"
    ) {
        assert_eq!(crate::commands::test_helpers::database_rows(&conn), before);
    } else {
        assert_eq!(
            Trove::find_by_name(&conn, "a-summary-update").unwrap()[0].version,
            "2.0.0"
        );
    }
    if scenario == "sequence_apply" {
        let installed = Trove::find_by_name(&conn, "z-summary-update").unwrap();
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].version, "2.0.0");
    }
    if scenario == "mixed" {
        assert_eq!(
            Trove::find_by_name(&conn, "z-summary-failed").unwrap()[0].version,
            "1.0.0"
        );
    }
}

#[test]
fn update_summaries_in_terminal_pipe_and_no_color() {
    for (tty, no_color) in [(false, false), (false, true), (true, false), (true, true)] {
        for scenario in [
            "preview",
            "relation_preview",
            "relation_apply",
            "sequence_preview",
            "sequence_apply",
            "apply",
            "pending",
            "mixed",
            "noop",
        ] {
            let test =
                "commands::update::package::tests::summary_capture::update_summary_capture_child";
            let mut command = if tty {
                let mut command = Command::new("script");
                command
                    .args([
                        "-qec",
                        "exec \"$CONARY_UPDATE_EXE\" --exact \"$CONARY_UPDATE_TEST\" --nocapture",
                        "/dev/null",
                    ])
                    .env("CONARY_UPDATE_EXE", std::env::current_exe().unwrap())
                    .env("CONARY_UPDATE_TEST", test);
                command
            } else {
                let mut command = Command::new(std::env::current_exe().unwrap());
                command.args(["--exact", test, "--nocapture"]);
                command
            };
            command
                .env("CONARY_UPDATE_SUMMARY_CAPTURE", scenario)
                .env("TERM", "xterm")
                .env_remove("NO_COLOR")
                .env_remove("CLICOLOR_FORCE")
                .env_remove("RUST_LOG");
            if no_color {
                command.env("NO_COLOR", "1");
            }
            let output = command.output().unwrap();
            let stdout = String::from_utf8(output.stdout)
                .unwrap()
                .replace("\r\n", "\n");
            assert!(
                output.status.success(),
                "{scenario}: {stdout}\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
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
            if scenario == "noop" {
                assert!(frame.contains("pinned"), "{frame}");
                assert!(frame.contains("No eligible updates selected."), "{frame}");
                assert!(!frame.contains("up to date"), "{frame}");
                assert!(!frame.contains("Applied package changes:"), "{frame}");
                continue;
            }
            assert_eq!(
                frame.matches("Planned package changes:").count(),
                1,
                "{frame}"
            );
            for field in [
                "Package",
                "Version",
                "CCS release",
                "Architecture",
                "a-summary-update",
                "1.0.0 -> 2.0.0",
                "x86_64",
            ] {
                assert!(frame.contains(field), "{frame}");
            }
            if scenario.starts_with("sequence_") {
                let planned = frame
                    .split_once("Planned package changes:")
                    .unwrap()
                    .1
                    .split("Applied package changes:")
                    .next()
                    .unwrap();
                for field in [
                    "Update (1):",
                    "Install (1):",
                    "Remove (1):",
                    "z-summary-update",
                ] {
                    assert!(planned.contains(field), "{frame}");
                }
            }
            if scenario.starts_with("relation_") {
                let planned = frame
                    .split_once("Planned package changes:")
                    .unwrap()
                    .1
                    .split("Applied package changes:")
                    .next()
                    .unwrap();
                for field in [
                    "Remove (1):",
                    "Deconfigure (1):",
                    "summary-obsolete",
                    "summary-consumer",
                    "obsolete",
                ] {
                    assert!(planned.contains(field), "{frame}");
                }
            }
            if matches!(
                scenario,
                "preview" | "relation_preview" | "sequence_preview"
            ) {
                assert!(!frame.contains("Generation:"), "{frame}");
                assert!(!frame.contains("Applied package changes:"), "{frame}");
                continue;
            }
            assert_eq!(
                frame.matches("Applied package changes:").count(),
                1,
                "{frame}"
            );
            let applied = frame.split_once("Applied package changes:").unwrap().1;
            assert!(applied.contains("Updated (1):"), "{frame}");
            if scenario == "sequence_apply" {
                for field in [
                    "Updated (1):",
                    "Installed (1):",
                    "Removed (1):",
                    "z-summary-update",
                ] {
                    assert!(applied.contains(field), "{frame}");
                }
            }
            if scenario == "relation_apply" {
                for field in [
                    "Removed (1):",
                    "Deconfigured (1):",
                    "summary-obsolete",
                    "summary-consumer",
                ] {
                    assert!(applied.contains(field), "{frame}");
                }
            }
            assert!(
                applied.contains(if scenario == "pending" {
                    "Generation: publication pending"
                } else {
                    " published"
                }),
                "{frame}"
            );
            assert!(applied.contains("--db-path='"), "{frame}");
            if scenario == "mixed" {
                assert!(!applied.contains("z-summary-failed"), "{frame}");
                assert!(!applied.contains("Request rollback"), "{frame}");
            } else {
                assert!(applied.contains("rollback of latest changeset"), "{frame}");
            }
        }
    }
}
