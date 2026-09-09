// apps/conary/tests/installed_release_selector.rs
//! Installed `--release` selectors must resolve one exact CCS release record.
//!
//! The fixture seeds identical name/version/architecture identities that the
//! schema distinguishes only by `package_release`: `1`, `2`, and NULL.

pub mod common;

use conary_core::db::models::{Trove, TroveType};
use conary_core::repository::versioning::VersionScheme;
use std::path::Path;
use std::process::{Command, Output};

struct Fixture {
    temp: tempfile::TempDir,
    db: String,
    conn: rusqlite::Connection,
    release1: i64,
    release2: i64,
    unspecified: i64,
}

fn fixture() -> Fixture {
    let (temp, db, conn) = common::create_test_db();
    let ids = [Some("1"), Some("2"), None].map(|release| {
        let mut trove = Trove::new(
            "demo".into(),
            "1.0.0".into(),
            TroveType::Package,
            VersionScheme::Conary,
        );
        trove.package_release = release.map(str::to_owned);
        trove.architecture = Some("x86_64".into());
        trove.insert(&conn).unwrap()
    });
    Fixture {
        temp,
        db,
        conn,
        release1: ids[0],
        release2: ids[1],
        unspecified: ids[2],
    }
}

/// Run the real CLI with a child-owned environment and no inherited controls.
fn run(db: &str, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_conary"));
    command.args(args).args(["--db-path", db]);
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("NO_COLOR", "1");
    command.output().unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn failure(output: &Output) -> String {
    assert!(
        !output.status.success(),
        "expected failure, stdout: {}",
        stdout(output)
    );
    stderr(output)
}

fn pinned(conn: &rusqlite::Connection, id: i64) -> i64 {
    conn.query_row("SELECT pinned FROM troves WHERE id = ?1", [id], |row| {
        row.get(0)
    })
    .unwrap()
}

#[test]
fn list_info_selects_each_release_and_unspecified_none() {
    let f = fixture();
    let before = common::database_snapshot(&f.db);
    for (release, expected) in [
        ("1", "  Release: 1"),
        ("2", "  Release: 2"),
        ("none", "  Release: Unspecified"),
    ] {
        let output = run(&f.db, &["list", "--info", "demo", "--release", release]);
        assert!(output.status.success(), "{}", stderr(&output));
        let text = stdout(&output);
        assert!(text.contains("Name        : demo"), "{text}");
        assert!(text.contains(expected), "release {release}: {text}");
        assert_eq!(common::database_snapshot(&f.db), before);
    }
}

#[test]
fn omitted_release_is_ambiguous_and_names_every_release() {
    let f = fixture();
    let before = common::database_snapshot(&f.db);

    let output = run(&f.db, &["list", "--info", "demo"]);
    let text = failure(&output);
    assert!(
        text.contains("Multiple installed variants of 'demo' match the selector"),
        "{text}"
    );
    for expected in ["release 1", "release 2", "release none"] {
        assert!(text.contains(expected), "missing {expected}: {text}");
    }
    assert!(
        text.contains("Use --version, --release, and/or --arch to choose one."),
        "{text}"
    );
    assert_eq!(common::database_snapshot(&f.db), before);

    let output = run(&f.db, &["list", "demo"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    for expected in ["release=1", "release=2", "release=Unspecified"] {
        assert!(text.contains(expected), "missing {expected}: {text}");
    }
    assert!(text.contains("Total: 3 package(s)"), "{text}");
}

#[test]
fn pin_and_unpin_mutate_only_the_selected_release() {
    let f = fixture();
    for (release, id) in [("1", f.release1), ("none", f.unspecified)] {
        let output = run(&f.db, &["pin", "demo", "--release", release]);
        assert!(output.status.success(), "{}", stderr(&output));
        assert!(
            stdout(&output).contains("Pinned package 'demo' at version 1.0.0"),
            "{}",
            stdout(&output)
        );
        assert_eq!(pinned(&f.conn, id), 1, "release {release}");
        for other in [f.release1, f.release2, f.unspecified] {
            if other != id {
                assert_eq!(
                    pinned(&f.conn, other),
                    0,
                    "release {release} touched {other}"
                );
            }
        }

        let output = run(&f.db, &["unpin", "demo", "--release", release]);
        assert!(output.status.success(), "{}", stderr(&output));
        assert!(
            stdout(&output).contains("Unpinned package 'demo' (version 1.0.0)"),
            "{}",
            stdout(&output)
        );
        for other in [f.release1, f.release2, f.unspecified] {
            assert_eq!(pinned(&f.conn, other), 0);
        }
    }
}

#[test]
fn invalid_release_values_fail_before_database_creation() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("missing.db");
    let missing = missing.to_str().unwrap();
    for (value, reason) in [
        ("0", "release must be greater than zero"),
        ("abc", "release must be an unsigned decimal integer"),
        (
            "18446744073709551616",
            "release exceeds the supported integer range",
        ),
    ] {
        let output = run(missing, &["list", "demo", "--release", value]);
        let text = failure(&output);
        let invalid = format!("invalid package release '{value}'");
        assert!(text.contains(&invalid), "{text}");
        assert!(text.contains(reason), "{text}");
        assert!(output.stdout.is_empty(), "{:?}", stdout(&output));
        assert!(!Path::new(missing).exists(), "database was created");
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    }
}

#[test]
fn unsupported_selector_combinations_are_refused_without_state_change() {
    let f = fixture();
    let file = f.temp.path().join("payload.txt");
    std::fs::write(&file, b"payload").unwrap();
    let file = file.to_str().unwrap();
    let before = common::database_snapshot(&f.db);
    let cases: Vec<(Vec<&str>, &str)> = vec![
        (
            vec!["list", "--path", file, "--release", "1"],
            "cannot be used with --path",
        ),
        (
            vec!["list", "--pinned", "--release", "1"],
            "cannot be used with --pinned",
        ),
        (
            vec!["update", "@base", "--release", "1", "--dry-run"],
            "cannot be used with collection updates",
        ),
        (
            vec!["update", "--release", "1", "--dry-run"],
            "A package name is required with --version, --release, or --arch for update",
        ),
    ];
    for (args, expected) in &cases {
        let output = run(&f.db, args);
        let text = failure(&output);
        assert!(text.contains(expected), "args {args:?}: {text}");
        assert_eq!(
            common::database_snapshot(&f.db),
            before,
            "args {args:?} mutated state"
        );
    }
}

#[test]
fn release_combines_with_version_and_arch_selectors() {
    let f = fixture();
    let before = common::database_snapshot(&f.db);
    let output = run(
        &f.db,
        &[
            "list",
            "--info",
            "demo",
            "--version",
            "1.0.0",
            "--arch",
            "x86_64",
            "--release",
            "2",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("Version     : 1.0.0"), "{text}");
    assert!(text.contains("Architecture: x86_64"), "{text}");
    assert!(text.contains("  Release: 2"), "{text}");
    assert_eq!(common::database_snapshot(&f.db), before);
}

#[test]
fn pinned_release_refusal_and_read_only_queries_preserve_state() {
    let f = fixture();
    let output = run(&f.db, &["pin", "demo", "--release", "1"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(pinned(&f.conn, f.release1), 1);
    let before = common::database_snapshot(&f.db);

    let output = run(&f.db, &["query", "whatbreaks", "demo", "--release", "1"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stdout(&output)
            .contains("Package 'demo' is pinned and remove would be refused before mutation."),
        "{}",
        stdout(&output)
    );
    assert_eq!(common::database_snapshot(&f.db), before);

    let output = run(&f.db, &["query", "scripts", "demo", "--release", "2"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stdout(&output).contains("Installed package: demo 1.0.0 [x86_64] release=2"),
        "{}",
        stdout(&output)
    );
    assert_eq!(common::database_snapshot(&f.db), before);

    let output = run(&f.db, &["remove", "demo", "--release", "1", "--yes"]);
    let text = failure(&output);
    assert!(
        text.contains(
            "Package 'demo' is pinned and cannot be removed. Use 'conary unpin demo' first."
        ),
        "{text}"
    );
    assert_eq!(pinned(&f.conn, f.release1), 1);
    assert_eq!(common::database_snapshot(&f.db), before);
}
