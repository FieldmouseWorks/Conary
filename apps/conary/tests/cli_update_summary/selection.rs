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
        "1".into(),
        TroveType::Collection,
        conary_core::repository::versioning::VersionScheme::Conary,
    );
    let collection_id = collection.insert(&conn).unwrap();
    CollectionMember::new(collection_id, "pinned".into())
        .insert(&conn)
        .unwrap();
    let mut trove = Trove::new(
        "pinned".into(),
        "1".into(),
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
                text.contains("pinned 1 [x86_64]  pinned; not checked"),
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
