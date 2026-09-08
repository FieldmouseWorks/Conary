// apps/conary/src/commands/install/report/tests.rs

use super::*;
use crate::commands::{InstallOptions, SandboxMode, test_helpers};
use conary_core::ccs::builder::{CcsBuilder, write_signed_current_ccs_package};
use conary_core::ccs::manifest::{CcsManifest, Platform};
use conary_core::db::models::{InstallReason, TroveType};
use conary_core::repository::versioning::VersionScheme;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn artifact(dir: &Path, name: &str, version: &str, ccs: bool, dependency: bool) -> PathBuf {
    if !ccs {
        let mut builder =
            rpm::PackageBuilder::new(name, version, "MIT", "x86_64", "summary fixture");
        if dependency {
            builder.requires(rpm::Dependency::any("summary-dependency"));
        }
        builder
            .with_file_contents(
                b"fixture\n".to_vec(),
                rpm::FileOptions::new(format!("/usr/share/{name}/data"))
                    .permissions(0o644)
                    .user("summary-user")
                    .group("summary-group"),
            )
            .unwrap();
        let path = dir.join(format!("{name}-{version}.rpm"));
        builder.build().unwrap().write_file(&path).unwrap();
        return path;
    }
    let source = dir.join(format!("src-{name}-{version}"));
    std::fs::create_dir_all(source.join("usr/share").join(name)).unwrap();
    std::fs::write(
        source.join("usr/share").join(name).join("data"),
        b"fixture\n",
    )
    .unwrap();
    let mut manifest = CcsManifest::new_minimal(name, version);
    manifest.package.platform = Some(Platform {
        os: "linux".into(),
        arch: Some("x86_64".into()),
        libc: "gnu".into(),
        abi: None,
    });
    let result = CcsBuilder::new(manifest, &source).unwrap().build().unwrap();
    let path = dir.join(format!("{name}-{version}.ccs"));
    let key = crate::commands::ccs::load_or_create_local_dev_key().unwrap();
    write_signed_current_ccs_package(&result, &path, &key, true).unwrap();
    path
}

fn rows(conn: &rusqlite::Connection) -> Vec<(String, Vec<Vec<rusqlite::types::Value>>)> {
    let tables = conn
        .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    tables
        .into_iter()
        .map(|table| {
            let mut statement = conn
                .prepare(&format!("SELECT * FROM \"{}\"", table.replace('"', "\"\"")))
                .unwrap();
            let columns = statement.column_count();
            let rows = statement
                .query_map([], |row| {
                    (0..columns)
                        .map(|column| row.get(column))
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            (table, rows)
        })
        .collect()
}

#[tokio::test]
async fn install_summary_capture_child() {
    let Ok(scenario) = std::env::var("CONARY_INSTALL_CAPTURE") else {
        return;
    };
    let _mount = crate::commands::composefs_ops::test_mount_skip_guard();
    let (temp, db_path) = test_helpers::create_test_db();
    test_helpers::seed_test_bootable_runtime(Path::new(&db_path));
    let conn = conary_core::db::open(&db_path).unwrap();
    let base = Trove::find_by_name(&conn, "test-runtime-base").unwrap()[0]
        .id
        .unwrap();
    let (uid, gid) = (unsafe { libc::geteuid() }, unsafe { libc::getegid() });
    for (path, contents) in [
        (
            "/etc/passwd",
            format!(
                "root:x:0:0:root:/root:/bin/sh\nsummary-user:x:{uid}:{gid}:fixture:/:/sbin/nologin\n"
            ),
        ),
        ("/etc/group", format!("root:x:0:\nsummary-group:x:{gid}:\n")),
    ] {
        test_helpers::insert_test_regular_file_with_parents(
            &conn,
            &db_path,
            path,
            contents.as_bytes(),
            0o644,
            base,
            None,
        );
    }
    let ccs = scenario.contains("ccs");
    let preview = scenario.contains("preview");
    let failed = scenario.contains("failed");
    let canceled = scenario == "canceled_native";
    let package = artifact(temp.path(), "summary-incoming", "2.0.0", ccs, canceled);
    if canceled {
        let repository = test_helpers::insert_test_static_ccs_repository(
            &conn,
            "summary-repo",
            "http://127.0.0.1:1/unused",
        );
        let mut dep = conary_core::db::models::RepositoryPackage::new(
            repository,
            "summary-dependency".into(),
            "1.0.0-1".into(),
            VersionScheme::Rpm,
            "a".repeat(64),
            1,
            "http://127.0.0.1:1/unused.ccs".into(),
        );
        dep.architecture = Some("x86_64".into());
        dep.source_profile = Some("fedora-44".into());
        dep.insert(&conn).unwrap();
    }
    if scenario.contains("upgrade") || failed {
        let version = if failed {
            if ccs { "2.0.0" } else { "2.0.0-1" }
        } else if ccs {
            "1.0.0"
        } else {
            "1.0.0-1"
        };
        let mut old = Trove::new(
            "summary-incoming".into(),
            version.into(),
            TroveType::Package,
            if ccs {
                VersionScheme::Conary
            } else {
                VersionScheme::Rpm
            },
        );
        old.architecture = Some("x86_64".into());
        if ccs {
            old.package_release = Some("1".into());
        }
        old.insert(&conn).unwrap();
    }
    let _failure = scenario.contains("pending").then(|| {
        crate::commands::composefs_ops::test_forced_generation_rebuild_failure_guard(
            "forced install summary publication failure",
        )
    });
    let before = rows(&conn);
    println!("FRAME_BEGIN");
    let result = if scenario.starts_with("batch") {
        let second = artifact(temp.path(), "summary-second", "3.0.0", false, false);
        let packages = [&package, &second]
            .into_iter()
            .map(|path| {
                super::super::prepare_package_for_batch(
                    path,
                    &db_path,
                    InstallReason::Explicit,
                    "summary fixture",
                    false,
                    None,
                )
                .unwrap()
                .unwrap()
            })
            .collect();
        let installer = super::super::BatchInstaller::new(&db_path, SandboxMode::Always);
        if preview {
            InstallReport {
                planned: installer.preview_batch(packages).unwrap(),
                commits: Vec::new(),
            }
            .render(&db_path, true);
            Ok(())
        } else {
            installer.install_batch(packages)
        }
    } else {
        crate::commands::cmd_install_cli(
            package.to_str().unwrap(),
            InstallOptions {
                db_path: &db_path,
                root: temp.path().to_str().unwrap(),
                dry_run: preview,
                no_deps: !canceled,
                yes: !canceled,
                sandbox_mode: SandboxMode::Always,
                ..Default::default()
            },
        )
        .await
    };
    println!("FRAME_END");
    if failed {
        assert!(result.is_err(), "expected duplicate-identity refusal");
    } else {
        result.unwrap();
    }
    if preview || failed || canceled {
        assert_eq!(
            rows(&conn),
            before,
            "preview/refusal/cancellation mutated database"
        );
    } else {
        let installed = Trove::find_by_name(&conn, "summary-incoming").unwrap();
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].version, if ccs { "2.0.0" } else { "2.0.0-1" });
        let pending =
            conary_core::db::models::GenerationPublication::pending_recoverable(&conn).unwrap();
        assert_eq!(!pending.is_empty(), scenario.contains("pending"));
    }
}

#[test]
fn install_summary_commands_in_terminal_pipe_and_no_color() {
    for (tty, no_color) in [(false, false), (false, true), (true, false), (true, true)] {
        for scenario in [
            "native",
            "preview_native",
            "upgrade_native",
            "pending_native",
            "failed_native",
            "ccs",
            "preview_ccs",
            "upgrade_ccs",
            "pending_ccs",
            "failed_ccs",
            "batch",
            "batch_preview",
            "batch_pending",
            "canceled_native",
        ] {
            let test = "commands::install::report::tests::install_summary_capture_child";
            let mut command = if tty {
                let mut command = Command::new("script");
                command
                    .args([
                        "-qec",
                        "exec \"$CONARY_INSTALL_EXE\" --exact \"$CONARY_INSTALL_TEST\" --nocapture",
                        "/dev/null",
                    ])
                    .env("CONARY_INSTALL_EXE", std::env::current_exe().unwrap())
                    .env("CONARY_INSTALL_TEST", test);
                command
            } else {
                let mut command = Command::new(std::env::current_exe().unwrap());
                command.args(["--exact", test, "--nocapture"]);
                command
            };
            command
                .env("CONARY_INSTALL_CAPTURE", scenario)
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
            if scenario == "canceled_native" {
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
            if scenario.contains("failed") || scenario == "canceled_native" {
                assert!(!frame.contains("Applied package changes:"), "{frame}");
                assert!(!frame.contains("Generation:"), "{frame}");
                if scenario == "canceled_native" {
                    assert!(frame.contains("Cancelled."), "{frame}");
                }
                continue;
            }
            let preview = scenario.contains("preview");
            let heading = if preview {
                "Planned package changes:"
            } else {
                "Applied package changes:"
            };
            assert_eq!(frame.matches(heading).count(), 1, "{frame}");
            for field in [
                "Package",
                "Version",
                "CCS release",
                "Architecture",
                "summary-incoming",
                "x86_64",
            ] {
                assert!(frame.contains(field), "{frame}");
            }
            if scenario.contains("upgrade") {
                assert!(frame.contains(" -> "), "{frame}");
                assert!(frame.contains("Updated (1):"), "{frame}");
            }
            if scenario.starts_with("batch") {
                assert!(frame.contains("summary-second"), "{frame}");
            }
            if preview {
                assert!(frame.contains("Dry run:"), "{frame}");
                assert!(!frame.contains("Generation:"), "{frame}");
            } else {
                assert!(frame.contains("Changeset:"), "{frame}");
                assert!(frame.contains("--db-path='"), "{frame}");
                assert!(
                    frame.contains(if scenario.contains("pending") {
                        "Generation: publication pending"
                    } else {
                        " published"
                    }),
                    "{frame}"
                );
                assert_eq!(
                    frame.contains("Request rollback of latest changeset")
                        || frame.contains("After publication, request rollback"),
                    !scenario.starts_with("batch"),
                    "{frame}"
                );
            }
        }
    }
}
