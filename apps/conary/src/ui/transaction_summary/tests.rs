// apps/conary/src/ui/transaction_summary/tests.rs

use super::*;
use conary_core::db::models::TroveType;
use conary_core::repository::versioning::VersionScheme;

fn publication(pending: bool) -> PublicationOutcome {
    PublicationOutcome {
        generation_number: (!pending).then_some(12),
        state_number: (!pending).then_some(12),
        needs_publication: pending,
        retry_command: None,
        failure_reason: None,
        completed_debts: usize::from(!pending),
    }
}

#[test]
fn grouped_changes_preserve_versions_releases_and_variants() {
    let mut old = TroveSnapshot::test_package("demo", "1:2.0-3", Vec::new());
    old.package_release = Some("4".into());
    old.architecture = Some("aarch64".into());
    let mut new = Trove::new(
        "demo".into(),
        "1:3.0-1".into(),
        TroveType::Package,
        VersionScheme::Conary,
    );
    new.architecture = Some("x86_64".into());
    let lines =
        change_lines(&[PackageChange::restored(&old), PackageChange::removed(&new)]).join("\n");
    assert_eq!(
        console::strip_ansi_codes(&lines),
        concat!(
            "Applied package changes:\n",
            "  Removed (1):\n",
            "    Package  Version  CCS release  Architecture\n",
            "    demo     1:3.0-1  -            x86_64\n",
            "  Restored (1):\n",
            "    Package  Version  CCS release  Architecture\n",
            "    demo     1:2.0-3  4            aarch64"
        )
    );
}

#[test]
fn displayed_identity_cannot_inject_rows_or_terminal_controls() {
    let entry = PackageChange {
        change: Change::Remove,
        name: "demo\n[ok]",
        version: "2\x1b[2J",
        release: None,
        architecture: Some("x\t64"),
        reason: None,
    };
    let lines = change_lines(&[entry]).join("\n");
    let text = console::strip_ansi_codes(&lines);
    assert_eq!(text.lines().count(), 4);
    assert!(text.contains("demo\\n[ok]"), "{text}");
    assert!(text.contains("2\\u{1b}[2J"), "{text}");
    assert!(text.contains("x\\t64"), "{text}");
}

#[test]
fn closing_distinguishes_pending_published_and_missing_generation() {
    for (outcome, expected) in [
        (publication(true), "publication pending"),
        (publication(false), "12 published"),
    ] {
        let lines = closing_lines(42, &outcome, "/tmp/fixture.db").join("\n");
        let text = console::strip_ansi_codes(&lines);
        assert!(text.contains("Changeset: 42"), "{text}");
        assert!(text.contains(&format!("Generation: {expected}")), "{text}");
        assert!(
            text.contains("conary system history --db-path='/tmp/fixture.db'"),
            "{text}"
        );
    }
    let mut missing = publication(false);
    missing.generation_number = None;
    let lines = closing_lines(42, &missing, "/tmp/fixture.db").join("\n");
    assert!(lines.contains("not reported"));
    assert!(!lines.contains("0 published"));
    assert_eq!(
        change_lines(&[]).last().unwrap(),
        "  No package identities changed."
    );
}

#[test]
fn recovery_command_preserves_shell_arguments_without_execution() {
    let path = "/tmp/a ' quoted $(exit 19) `exit 20` ; database.db";
    let command = database_command("conary system history", path);
    let script = format!("conary() {{ printf '%s\\0' \"$@\"; }}\n{command}");
    let output = std::process::Command::new("sh")
        .args(["-c", &script])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let args: Vec<_> = output.stdout.split(|byte| *byte == 0).collect();
    assert_eq!(
        args,
        [
            b"system".as_slice(),
            b"history",
            format!("--db-path={path}").as_bytes(),
            b""
        ]
    );
    let default = conary_core::runtime_root::ConaryRuntimeRoot::default();
    assert_eq!(
        database_command("conary system history", default.db_path().to_str().unwrap()),
        "conary system history"
    );
    assert_eq!(
        database_command("conary system history", "/tmp/line\nbreak"),
        "conary system history --db-path <PATH> (use the same database path)"
    );
}

#[cfg(feature = "test-hooks")]
#[path = "capture.rs"]
mod capture;
