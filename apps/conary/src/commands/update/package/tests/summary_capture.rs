// apps/conary/src/commands/update/package/tests/summary_capture.rs

use super::*;
use std::io::Write;
use std::process::{Command, Stdio};

#[path = "summary_capture/cancellation.rs"]
mod cancellation;

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
    let cancelled = scenario.starts_with("cancel_");
    if cancelled {
        cancellation::add_candidate(&conn, temp.path(), &db_path, &scenario).await;
    } else {
        add_candidate(
            &conn,
            temp.path(),
            "a-summary-update",
            None,
            scenario.starts_with("relation_"),
            scenario
                .starts_with("sequence_")
                .then_some("z-summary-update < 2.0.0"),
            scenario.starts_with("named_"),
        );
    }
    if scenario == "mixed" || scenario == "preflight" {
        add_candidate(
            &conn,
            temp.path(),
            "z-summary-failed",
            Some(if scenario == "preflight" {
                fixtures::FixtureFailure::MissingInterpreter
            } else {
                fixtures::FixtureFailure::Lifecycle
            }),
            false,
            None,
            false,
        );
    }
    if scenario.starts_with("sequence_") {
        add_candidate(
            &conn,
            temp.path(),
            "z-summary-update",
            None,
            false,
            None,
            false,
        );
    }
    if scenario.starts_with("fallback_") {
        add_candidate(
            &conn,
            temp.path(),
            "z-summary-update",
            None,
            false,
            Some("a-summary-update <= 2.0.0"),
            false,
        );
        fixtures::add_failing_delta(&conn, temp.path(), scenario != "fallback_apply");
    }
    if scenario.starts_with("named_") {
        let base = Trove::find_by_name(&conn, "test-runtime-base").unwrap()[0]
            .id
            .unwrap();
        for (path, contents) in [
            ("/etc/passwd", "root:x:0:0:root:/root:/bin/sh\n"),
            ("/etc/group", "root:x:0:\n"),
        ] {
            crate::commands::test_helpers::insert_test_regular_file_with_parents(
                &conn,
                Path::new(&db_path),
                path,
                contents.as_bytes(),
                0o644,
                base,
                None,
            );
        }
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
            "preview"
                | "relation_preview"
                | "sequence_preview"
                | "fallback_preview"
                | "named_preview"
        ),
        SandboxMode::Always,
        None,
        !cancelled,
        None,
        None,
        true,
    )
    .await;
    println!("FRAME_END");
    if cancelled {
        let error = result.expect_err("dependency cancellation must stop the update");
        assert!(error.to_string().contains("Update cancelled"), "{error:#}");
        for name in ["a-summary-update", "z-summary-update"] {
            assert_eq!(
                Trove::find_by_name(&conn, name).unwrap()[0].version,
                "1.0.0"
            );
        }
        assert!(
            Trove::find_by_name(&conn, "summary-dependency")
                .unwrap()
                .is_empty()
        );
        let stats: (i64, i64) = conn
            .query_row(
                "SELECT full_downloads, deltas_applied FROM delta_stats ORDER BY id DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(stats, (2, 0));
        let temporary = Path::new(&db_path).parent().unwrap().join("tmp");
        assert!(
            std::fs::read_dir(temporary).unwrap().next().is_none(),
            "cancelled update left temporary delta artifacts"
        );
        return;
    }
    if scenario == "mixed" || scenario == "preflight" {
        let error = result.expect_err("later lifecycle/preflight failure unexpectedly succeeded");
        if scenario == "preflight" {
            assert!(format!("{error:#}").contains("preflight"), "{error:#}");
            assert!(
                conary_core::db::models::FileEntry::find_by_path(
                    &conn,
                    "/usr/bin/z-summary-failed"
                )
                .unwrap()
                .is_none()
            );
        }
    } else {
        assert_eq!(
            result.unwrap(),
            match scenario.as_str() {
                "sequence_preview" | "fallback_preview" =>
                    crate::commands::update::outcome::UpdateOutcome::Planned { packages: 2 },
                "sequence_apply" | "fallback_download" | "fallback_apply" =>
                    crate::commands::update::outcome::UpdateOutcome::Applied { packages: 2 },
                "preview" | "relation_preview" | "named_preview" =>
                    crate::commands::update::outcome::UpdateOutcome::Planned { packages: 1 },
                "noop" => crate::commands::update::outcome::UpdateOutcome::NoChanges,
                _ => crate::commands::update::outcome::UpdateOutcome::Applied { packages: 1 },
            }
        );
    }
    if matches!(
        scenario.as_str(),
        "preview"
            | "relation_preview"
            | "sequence_preview"
            | "fallback_preview"
            | "named_preview"
            | "noop"
    ) {
        assert_eq!(crate::commands::test_helpers::database_rows(&conn), before);
    } else if scenario.starts_with("fallback_") {
        assert!(
            Trove::find_by_name(&conn, "a-summary-update")
                .unwrap()
                .is_empty(),
            "deferred delta fallback reinstalled the removed target"
        );
        assert_eq!(
            Trove::find_by_name(&conn, "z-summary-update").unwrap()[0].version,
            "2.0.0"
        );
    } else {
        assert_eq!(
            Trove::find_by_name(&conn, "a-summary-update").unwrap()[0].version,
            "2.0.0"
        );
    }
    if matches!(
        scenario.as_str(),
        "mixed"
            | "preflight"
            | "published"
            | "pending"
            | "sequence_apply"
            | "fallback_download"
            | "fallback_apply"
            | "named_apply"
    ) {
        let (prepared, saved): (i32, i64) = conn.query_row("SELECT full_downloads, total_bytes_saved FROM delta_stats ORDER BY id DESC LIMIT 1", [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        let selected = if matches!(
            scenario.as_str(),
            "mixed" | "preflight" | "sequence_apply" | "fallback_download" | "fallback_apply"
        ) {
            2
        } else {
            1
        };
        assert_eq!(
            prepared, selected,
            "artifact preparation must include targets whose later apply failed"
        );
        assert_eq!(
            saved, 0,
            "full-artifact preview has already consumed the full package"
        );
    }
    if scenario == "named_apply" {
        let file =
            conary_core::db::models::FileEntry::find_by_path(&conn, "/usr/bin/a-summary-update")
                .unwrap()
                .unwrap();
        assert_eq!(file.node.uid, u64::from(unsafe { libc::geteuid() }));
        assert_eq!(file.node.gid, u64::from(unsafe { libc::getegid() }));
        assert_eq!(
            file.node.source.user,
            conary_core::payload::PayloadIdentity::Named {
                name: "summary-late-user".into()
            }
        );
    }
    if scenario == "sequence_apply" {
        let installed = Trove::find_by_name(&conn, "z-summary-update").unwrap();
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].version, "2.0.0");
    }
    if scenario == "mixed" || scenario == "preflight" {
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
            "fallback_preview",
            "fallback_download",
            "fallback_apply",
            "named_preview",
            "named_apply",
            "apply",
            "pending",
            "mixed",
            "preflight",
            "noop",
            "cancel_full",
            "cancel_ccs",
            "cancel_delta",
            "cancel_fallback",
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
            crate::test_hooks::clear_inherited_hooks(&mut command);
            command
                .env("CONARY_UPDATE_SUMMARY_CAPTURE", scenario)
                .env("TERM", "xterm")
                .env_remove("NO_COLOR")
                .env_remove("CLICOLOR_FORCE")
                .env_remove("RUST_LOG");
            if no_color {
                command.env("NO_COLOR", "1");
            }
            let mut child = command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            if scenario.starts_with("cancel_") {
                child.stdin.take().unwrap().write_all(b"n\n").unwrap();
            } else {
                drop(child.stdin.take());
            }
            let output = child.wait_with_output().unwrap();
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
            if scenario.starts_with("cancel_") {
                assert!(frame.contains("Cancelled."), "{frame}");
                assert!(!frame.contains("Applied package changes:"), "{frame}");
                assert!(!frame.contains("Rollback:"), "{frame}");
                assert!(!frame.contains("[ok] a-summary-update"), "{frame}");
                continue;
            }
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
            if scenario.starts_with("fallback_") {
                let planned = frame
                    .split_once("Planned package changes:")
                    .unwrap()
                    .1
                    .split("Applied package changes:")
                    .next()
                    .unwrap();
                for field in [
                    "Update (2):",
                    "Remove (1):",
                    "a-summary-update",
                    "z-summary-update",
                ] {
                    assert!(planned.contains(field), "{frame}");
                }
                assert!(!planned.contains("Install (1):"), "{frame}");
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
                "preview"
                    | "relation_preview"
                    | "sequence_preview"
                    | "fallback_preview"
                    | "named_preview"
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
            assert!(
                applied.contains(if scenario.starts_with("fallback_") {
                    "Updated (2):"
                } else {
                    "Updated (1):"
                }),
                "{frame}"
            );
            if scenario.starts_with("fallback_") {
                assert!(applied.contains("Removed (1):"), "{frame}");
                assert!(!applied.contains("Installed (1):"), "{frame}");
                assert!(frame.contains("Delta failures: 1"), "{frame}");
            }
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
            if scenario == "mixed" || scenario == "preflight" {
                assert!(!applied.contains("z-summary-failed"), "{frame}");
                assert!(!applied.contains("Request rollback"), "{frame}");
            } else {
                assert!(applied.contains("rollback of latest changeset"), "{frame}");
            }
        }
    }
}
