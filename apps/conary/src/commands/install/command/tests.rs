// apps/conary/src/commands/install/command/tests.rs

use super::*;
use crate::commands::test_helpers::create_test_db;
use conary_core::db::models::{InstallReason, InstallSource, Trove, TroveType};
use conary_core::repository::versioning::VersionScheme;

fn dependency(conn: &rusqlite::Connection, version: &str, architecture: &str) -> i64 {
    let mut trove = Trove::new_with_source(
        "promotion-fixture".to_string(),
        version.to_string(),
        TroveType::Package,
        InstallSource::Repository,
        VersionScheme::Rpm,
    );
    trove.architecture = Some(architecture.to_string());
    trove.install_reason = InstallReason::Dependency;
    trove.selection_reason = Some("Required by another package".to_string());
    trove.insert(conn).unwrap()
}

fn data_version(conn: &rusqlite::Connection) -> i64 {
    conn.query_row("PRAGMA data_version", [], |row| row.get(0))
        .unwrap()
}

#[tokio::test]
async fn dry_run_preserves_dependency_promotion_state_even_with_yes() {
    for yes in [false, true] {
        let (temp, db_path) = create_test_db();
        let conn = conary_core::db::open(&db_path).unwrap();
        let id = dependency(&conn, "1.0-1", "x86_64");
        let before = data_version(&conn);

        cmd_install(
            "promotion-fixture",
            InstallOptions {
                db_path: &db_path,
                root: temp.path().to_str().unwrap(),
                dry_run: true,
                yes,
                selection_reason: Some("Requested preview"),
                ..InstallOptions::default()
            },
        )
        .await
        .unwrap();

        let installed = Trove::find_by_id(&conn, id).unwrap().unwrap();
        assert_eq!(installed.install_reason, InstallReason::Dependency);
        assert_eq!(
            installed.selection_reason.as_deref(),
            Some("Required by another package")
        );
        assert_eq!(
            data_version(&conn),
            before,
            "dry run committed database changes"
        );
    }
}

#[tokio::test]
async fn install_promotes_only_the_selected_dependency_variant() {
    for selection_reason in [None, Some("Keep this package")] {
        let (temp, db_path) = create_test_db();
        let conn = conary_core::db::open(&db_path).unwrap();
        let selected = dependency(&conn, "1.0-1", "x86_64");
        let other_version = dependency(&conn, "2.0-1", "x86_64");
        let other_arch = dependency(&conn, "1.0-1", "i686");

        for dry_run in [true, false] {
            let before = data_version(&conn);
            cmd_install(
                "promotion-fixture",
                InstallOptions {
                    db_path: &db_path,
                    root: temp.path().to_str().unwrap(),
                    version: Some("1.0-1".to_string()),
                    architecture: Some("x86_64".to_string()),
                    dry_run,
                    yes: true,
                    selection_reason,
                    ..InstallOptions::default()
                },
            )
            .await
            .unwrap();

            for id in [selected, other_version, other_arch] {
                let installed = Trove::find_by_id(&conn, id).unwrap().unwrap();
                let promoted = id == selected && !dry_run;
                assert_eq!(
                    installed.install_reason,
                    if promoted {
                        InstallReason::Explicit
                    } else {
                        InstallReason::Dependency
                    }
                );
                assert_eq!(
                    installed.selection_reason.as_deref(),
                    Some(if promoted {
                        selection_reason.unwrap_or("Explicitly installed by user")
                    } else {
                        "Required by another package"
                    })
                );
            }
            if dry_run {
                assert_eq!(data_version(&conn), before);
            }
        }
    }
}

#[tokio::test]
async fn ambiguous_dependency_promotion_refuses_without_writes() {
    let (temp, db_path) = create_test_db();
    let conn = conary_core::db::open(&db_path).unwrap();
    dependency(&conn, "1.0-1", "x86_64");
    dependency(&conn, "1.0-1", "i686");
    let before = data_version(&conn);

    for dry_run in [true, false] {
        let error = cmd_install(
            "promotion-fixture",
            InstallOptions {
                db_path: &db_path,
                root: temp.path().to_str().unwrap(),
                dry_run,
                yes: true,
                ..InstallOptions::default()
            },
        )
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("select the exact variant"),
            "{error:#}"
        );
        assert_eq!(data_version(&conn), before);
    }
}
