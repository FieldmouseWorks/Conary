// apps/conary/tests/cli_installed_list.rs

#![cfg(test)]
//! Ordinary list output preserves typed records without terminal or count ambiguity.

pub mod common;

use conary_core::db::models::{Trove, TroveType};
use conary_core::repository::versioning::VersionScheme;
use std::process::Command;

fn capture(db: &str, args: &[&str], tty: bool, no_color: bool) -> String {
    let mut all_args = vec!["list", "--db-path", db];
    all_args.extend_from_slice(args);
    let mut command = if tty {
        let arguments = (0..all_args.len())
            .map(|index| format!("\"$CONARY_LIST_ARG_{index}\""))
            .collect::<Vec<_>>()
            .join(" ");
        let mut command = Command::new("script");
        command
            .args([
                "-qec",
                &format!("exec \"$CONARY_LIST_EXE\" {arguments}"),
                "/dev/null",
            ])
            .env("CONARY_LIST_EXE", env!("CARGO_BIN_EXE_conary"));
        for (index, argument) in all_args.iter().enumerate() {
            command.env(format!("CONARY_LIST_ARG_{index}"), argument);
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

#[test]
fn list_distinguishes_record_types_and_escapes_record_and_database_fields() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("selected '\n\x1b[2J.db");
    let db = path.to_str().unwrap();
    conary_core::db::init(db).unwrap();
    let conn = conary_core::db::open(db).unwrap();
    // Seed out of name order through the registered core connection and model.
    let mut collection = Trove::new(
        "zeta-group".into(),
        "1.0.0".into(),
        TroveType::Collection,
        VersionScheme::Conary,
    );
    collection.architecture = Some("arch\n[off] forged\x1b[2J".into());
    collection.insert(&conn).unwrap();
    let mut package = Trove::new(
        "bravo\n[ok] forged\x1b[2J".into(),
        "1.0.0".into(),
        TroveType::Package,
        VersionScheme::Conary,
    );
    package.package_release = Some("7".into());
    package.architecture = Some("x86_64".into());
    package.insert(&conn).unwrap();
    Trove::new(
        "alpha:runtime".into(),
        "1.0.0".into(),
        TroveType::Component,
        VersionScheme::Conary,
    )
    .insert(&conn)
    .unwrap();
    let before = common::database_snapshot(db);
    for tty in [false, true] {
        for no_color in [false, true] {
            let text = capture(db, &[], tty, no_color);
            assert_eq!(
                text,
                format!(
                    concat!(
                        "Installed records:\n  Database: {}/selected '\\n\\u{{1b}}[2J.db\n",
                        "[info]     alpha:runtime  1.0.0  Type: component  Release: Unspecified  Architecture: Unspecified\n",
                        "[info]     bravo\\n[ok] forged\\u{{1b}}[2J  1.0.0  Type: package  Release: 7  Architecture: x86_64\n",
                        "[info]     zeta-group  1.0.0  Type: collection  Release: Unspecified  Architecture: arch\\n[off] forged\\u{{1b}}[2J\n",
                        "  Records: 3\n  Packages: 1\n  Components: 1\n  Collections: 1\n"
                    ),
                    temp.path().display()
                )
            );
            assert_eq!(common::database_snapshot(db), before);
        }
    }
}

#[test]
fn name_and_release_selection_retain_each_exact_installed_record() {
    let (_temp, db, conn) = common::create_test_db();
    for release in [Some("1"), Some("2"), None] {
        let mut trove = Trove::new(
            "demo".into(),
            "1.0.0".into(),
            TroveType::Package,
            VersionScheme::Conary,
        );
        trove.package_release = release.map(str::to_owned);
        trove.architecture = Some("x86_64".into());
        trove.insert(&conn).unwrap();
    }
    let before = common::database_snapshot(&db);
    for tty in [false, true] {
        for no_color in [false, true] {
            let text = capture(&db, &["demo"], tty, no_color);
            assert!(text.starts_with(&format!(
                "Installed records:\n  Database: {db}\n  Name: demo\n"
            )));
            let mut rows: Vec<_> = text
                .lines()
                .filter(|line| line.starts_with("[info]"))
                .collect();
            rows.sort_unstable();
            assert_eq!(
                rows,
                [
                    "[info]     demo  1.0.0  Type: package  Release: 1  Architecture: x86_64",
                    "[info]     demo  1.0.0  Type: package  Release: 2  Architecture: x86_64",
                    "[info]     demo  1.0.0  Type: package  Release: Unspecified  Architecture: x86_64",
                ]
            );
            assert!(
                text.ends_with("  Records: 3\n  Packages: 3\n  Components: 0\n  Collections: 0\n")
            );
            for (selector, displayed) in [("1", "1"), ("2", "2"), ("none", "Unspecified")] {
                assert_eq!(
                    capture(
                        &db,
                        &[
                            "demo",
                            "--version",
                            "1.0.0",
                            "--release",
                            selector,
                            "--arch",
                            "x86_64"
                        ],
                        tty,
                        no_color
                    ),
                    format!(
                        "Installed records:\n  Database: {db}\n  Name: demo\n[info]     demo  1.0.0  Type: package  Release: {displayed}  Architecture: x86_64\n  Records: 1\n  Packages: 1\n  Components: 0\n  Collections: 0\n"
                    )
                );
                assert_eq!(common::database_snapshot(&db), before);
            }
        }
    }
}

#[test]
fn empty_list_separates_requested_name_from_installed_results() {
    let (_temp, db, _conn) = common::create_test_db();
    let before = common::database_snapshot(&db);
    for tty in [false, true] {
        for no_color in [false, true] {
            assert_eq!(
                capture(&db, &[], tty, no_color),
                format!(
                    "Installed records:\n  Database: {db}\nNo installed records.\n  Records: 0\n  Packages: 0\n  Components: 0\n  Collections: 0\n"
                )
            );
            assert_eq!(
                capture(
                    &db,
                    &["--", "--missing '\n[ok] forged\x1b[2J"],
                    tty,
                    no_color
                ),
                format!(
                    "Installed records:\n  Database: {db}\n  Name: --missing '\\n[ok] forged\\u{{1b}}[2J\nNo matching installed records.\n  Records: 0\n  Packages: 0\n  Components: 0\n  Collections: 0\n"
                )
            );
            assert_eq!(common::database_snapshot(&db), before);
        }
    }
}
