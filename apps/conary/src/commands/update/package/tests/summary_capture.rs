// apps/conary/src/commands/update/package/tests/summary_capture.rs

use super::*;
use std::process::Command;

fn add_candidate(conn: &rusqlite::Connection, dir: &Path, name: &str, fail: bool, relation: bool) {
    let mut bundle = rpm_upgrade_bundle(name, "2.0.0");
    if fail {
        bundle.entries[0].body = "error('forced update summary lifecycle failure')\n".into();
        bundle.entries[0].body_sha256 =
            conary_core::hash::sha256_prefixed(bundle.entries[0].body.as_bytes());
    }
    let artifact_dir = dir.join(name);
    std::fs::create_dir_all(&artifact_dir).unwrap();
    let relations = if relation {
        use conary_core::repository::{
            dependency_model::RepositoryRequirementKind, versioning::VersionScheme,
        };
        let mut old = Trove::new(
            "summary-obsolete".into(),
            "1".into(),
            TroveType::Package,
            VersionScheme::Rpm,
        );
        old.insert(conn).unwrap();
        let mut consumer = Trove::new(
            "summary-consumer".into(),
            "1".into(),
            TroveType::Package,
            VersionScheme::Debian,
        );
        consumer.debian_multi_arch =
            Some(conary_core::repository::dependency_model::DebianMultiArch::No);
        let consumer_id = consumer.insert(conn).unwrap();
        let dependency = conary_core::repository::requirement::parse_native_requirement(
            RepositoryRequirementKind::Depends,
            VersionScheme::Debian,
            "summary-obsolete",
        )
        .unwrap();
        conary_core::db::models::InstalledRequirementGroup::insert_groups(
            conn,
            consumer_id,
            VersionScheme::Debian,
            &[dependency],
        )
        .unwrap();
        let mut consumer_bundle = rpm_upgrade_bundle("summary-consumer", "1");
        consumer_bundle.source_format = SourceFormat::Deb;
        consumer_bundle.source_family = "debian".into();
        consumer_bundle.source_profile = Some("debian-13".into());
        consumer_bundle.source_release = Some("13".into());
        consumer_bundle.version_scheme = conary_core::ccs::native_lifecycle::VersionScheme::Deb;
        consumer_bundle.entries.clear();
        conary_core::db::models::InstalledNativeLifecycleBundle::new(
            consumer_id,
            None,
            &consumer_bundle,
        )
        .unwrap()
        .insert_or_replace(conn)
        .unwrap();
        vec![
            conary_core::repository::package_relation::parse_native_relation(
                RepositoryRequirementKind::Obsolete,
                VersionScheme::Rpm,
                "summary-obsolete",
            )
            .unwrap(),
        ]
    } else {
        Vec::new()
    };
    let path = build_test_ccs_package_with_relations(
        &artifact_dir,
        name,
        "2.0.0",
        Some(bundle),
        relations,
    );
    let bytes = std::fs::read(&path).unwrap();
    let (url, _) = serve_test_file(path);
    let repo = insert_test_static_ccs_repository(conn, name, &url);
    let mut old = Trove::new_with_source(
        name.into(),
        "1.0.0".into(),
        TroveType::Package,
        InstallSource::Repository,
        conary_core::repository::versioning::VersionScheme::Rpm,
    );
    old.architecture = Some("x86_64".into());
    old.source_profile = Some("fedora-44".into());
    old.installed_from_repository_id = Some(repo);
    old.insert(conn).unwrap();
    let mut candidate = RepositoryPackage::new(
        repo,
        name.into(),
        "2.0.0".into(),
        conary_core::repository::versioning::VersionScheme::Rpm,
        conary_core::hash::sha256(&bytes),
        bytes.len() as i64,
        url,
    );
    candidate.architecture = Some("x86_64".into());
    candidate.source_profile = Some("fedora-44".into());
    let id = candidate.insert(conn).unwrap();
    let mut resolution = PackageResolution::new(
        repo,
        name.into(),
        vec![ResolutionStrategy::RepositoryPackage {
            repository_package_id: id,
        }],
    );
    resolution.version = Some("2.0.0".into());
    resolution.primary_strategy = PrimaryStrategy::RepositoryPackage;
    resolution.insert(conn).unwrap();
}

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
    );
    if scenario == "mixed" {
        add_candidate(&conn, temp.path(), "z-summary-failed", true, false);
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
        matches!(scenario.as_str(), "preview" | "relation_preview"),
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
                "preview" | "relation_preview" =>
                    crate::commands::update::outcome::UpdateOutcome::Planned { packages: 1 },
                "noop" => crate::commands::update::outcome::UpdateOutcome::NoChanges,
                _ => crate::commands::update::outcome::UpdateOutcome::Applied { packages: 1 },
            }
        );
    }
    if matches!(scenario.as_str(), "preview" | "relation_preview" | "noop") {
        assert_eq!(crate::commands::test_helpers::database_rows(&conn), before);
    } else {
        assert_eq!(
            Trove::find_by_name(&conn, "a-summary-update").unwrap()[0].version,
            "2.0.0"
        );
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
            if matches!(scenario, "preview" | "relation_preview") {
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
