// apps/conary/tests/cli_update_summary/selection.rs

use super::*;

fn capture(db: &str, security: bool, tty: bool, no_color: bool) -> (bool, String) {
    let mut command = if tty {
        let mut command = Command::new("script");
        let script = if security {
            "exec \"$CONARY_SELECTION_EXE\" update @base --dry-run --security --db-path \"$CONARY_SELECTION_DB\""
        } else {
            "exec \"$CONARY_SELECTION_EXE\" update @base --dry-run --db-path \"$CONARY_SELECTION_DB\""
        };
        command
            .args(["-qec", script, "/dev/null"])
            .env("CONARY_SELECTION_EXE", env!("CARGO_BIN_EXE_conary"))
            .env("CONARY_SELECTION_DB", db);
        command
    } else {
        let mut command = Command::new(env!("CARGO_BIN_EXE_conary"));
        command.args(["update", "@base", "--dry-run", "--db-path", db]);
        if security {
            command.arg("--security");
        }
        command
    };
    command
        .env_remove("RUST_LOG")
        .env_remove("NO_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .env("TERM", "xterm");
    if no_color {
        command.env("NO_COLOR", "1");
    }
    let output = command.output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap()
    )
    .replace("\r\n", "\n");
    if no_color || !tty {
        assert!(!text.contains('\u{1b}'), "{text}");
    }
    (
        output.status.success(),
        console::strip_ansi_codes(&text).into_owned(),
    )
}

#[test]
fn pinned_members_are_not_reported_as_current() {
    let (_temp, db, conn) = super::common::create_test_db();
    let mut collection = Trove::new(
        "base".into(),
        "1.0.0".into(),
        TroveType::Collection,
        conary_core::repository::versioning::VersionScheme::Conary,
    );
    let collection_id = collection.insert(&conn).unwrap();
    CollectionMember::new(collection_id, "pinned".into())
        .insert(&conn)
        .unwrap();
    let mut trove = Trove::new(
        "pinned".into(),
        "1.0.0".into(),
        TroveType::Package,
        conary_core::repository::versioning::VersionScheme::Conary,
    );
    trove.pinned = true;
    trove.architecture = Some("x86_64".into());
    trove.insert(&conn).unwrap();
    drop(conn);
    let before = super::common::database_snapshot(&db);
    for security in [false, true] {
        for (tty, no_color) in [(false, false), (false, true), (true, false), (true, true)] {
            let (success, text) = capture(&db, security, tty, no_color);
            assert!(success, "{text}");
            assert!(!text.contains("up to date"), "{text}");
            assert!(text.contains("  Pinned packages: 1\n"), "{text}");
            assert!(
                text.contains("pinned 1.0.0 [x86_64]  pinned; not checked"),
                "{text}"
            );
            assert!(
                text.contains(if security {
                    "No eligible security updates selected."
                } else {
                    "No eligible updates selected."
                }),
                "{text}"
            );
            assert_eq!(super::common::database_snapshot(&db), before);
        }
    }
}

#[test]
fn empty_selection_preserves_each_reason_in_all_output_modes() {
    for (sql, expected, empty) in [
        (
            Some("DELETE FROM troves WHERE type = 'package'"),
            "  Uninstalled members: 2\n",
            false,
        ),
        (
            Some("DELETE FROM repository_packages"),
            "  Packages without eligible updates: 2\n",
            false,
        ),
        (
            Some("DELETE FROM collection_members"),
            "  Members: 0\n",
            true,
        ),
        (None, "  Externally managed packages: 2\n", false),
    ] {
        let (_temp, db) = fixture();
        let conn = conary_core::db::open(&db).unwrap();
        if let Some(sql) = sql {
            conn.execute(sql, []).unwrap();
        } else {
            for name in ["demo", "tools"] {
                let identity = conary_core::packages::InstalledPackageIdentity::eopkg(
                    name, name, "1.0", 1, "x86_64",
                )
                .unwrap();
                conn.execute("UPDATE troves SET install_source = ?1, native_package_identity_json = ?2 WHERE name = ?3",
                    rusqlite::params![InstallSource::AdoptedTrack.as_str(), serde_json::to_string(&identity).unwrap(), name]).unwrap();
            }
        }
        drop(conn);
        let before = common::database_snapshot(&db);
        for security in [false, true] {
            let mut previous = None;
            for (tty, no_color) in [(false, false), (false, true), (true, false), (true, true)] {
                let (success, text) = capture(&db, security, tty, no_color);
                assert!(success, "{text}");
                assert!(text.starts_with("Collection update selection\n"), "{text}");
                assert!(text.contains(expected), "{text}");
                if sql.is_none() {
                    assert!(text.contains("external authority:"), "{text}");
                    assert!(text.contains("conary system adopt --refresh"), "{text}");
                }
                assert!(text.contains("  Selected packages: 0\n"), "{text}");
                assert!(!text.contains("up to date"), "{text}");
                let closing = if empty {
                    "Collection has no members."
                } else if security {
                    "No eligible security updates selected."
                } else {
                    "No eligible updates selected."
                };
                assert!(text.contains(closing), "{text}");
                if let Some(previous) = &previous {
                    assert_eq!(&text, previous);
                }
                previous = Some(text);
                assert_eq!(common::database_snapshot(&db), before);
            }
        }
    }
}

#[test]
fn mixed_selection_keeps_skipped_members_visible_next_to_planned_updates() {
    let (_temp, db) = fixture();
    let conn = conary_core::db::open(&db).unwrap();
    conn.execute("UPDATE troves SET pinned = 1 WHERE name = 'demo'", [])
        .unwrap();
    let collection = Trove::find_by_name(&conn, "base").unwrap().pop().unwrap();
    for name in ["missing", "current"] {
        CollectionMember::new(collection.id.unwrap(), name.into())
            .insert(&conn)
            .unwrap();
    }
    let mut current = Trove::new(
        "current".into(),
        "1.0.0".into(),
        TroveType::Package,
        conary_core::repository::versioning::VersionScheme::Conary,
    );
    current.architecture = Some("x86_64".into());
    current.insert(&conn).unwrap();
    drop(conn);
    let before = common::database_snapshot(&db);
    for (tty, no_color) in [(false, false), (false, true), (true, false), (true, true)] {
        let (success, text) = capture(&db, false, tty, no_color);
        assert!(success, "{text}");
        for expected in [
            "  Members: 4\n",
            "  Selected packages: 1\n",
            "  Pinned packages: 1\n",
            "  Packages without eligible updates: 1\n",
            "  Uninstalled members: 1\n",
            "demo 1.0-1 [x86_64]  pinned; not checked",
            "tools 1.0-1 [x86_64]  selected for update",
            "current 1.0.0 [x86_64]  no eligible update",
            "missing  not installed",
            "  Planned packages: 1\n",
        ] {
            assert!(text.contains(expected), "missing {expected:?}: {text}");
        }
        assert!(!text.contains("No eligible updates selected."), "{text}");
        assert_eq!(common::database_snapshot(&db), before);
    }
}

#[test]
fn unavailable_security_metadata_is_an_error_without_an_empty_selection_claim() {
    let (_temp, db) = fixture();
    let before = common::database_snapshot(&db);
    for (tty, no_color) in [(false, false), (false, true), (true, false), (true, true)] {
        let (success, text) = capture(&db, true, tty, no_color);
        assert!(!success, "{text}");
        assert!(text.contains("Security metadata unavailable"), "{text}");
        assert!(text.contains("Cannot run security-only update"), "{text}");
        assert!(!text.contains("Collection update selection"), "{text}");
        assert!(
            !text.contains("No eligible security updates selected."),
            "{text}"
        );
        assert_eq!(common::database_snapshot(&db), before);
    }
}
