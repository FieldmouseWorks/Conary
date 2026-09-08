// apps/conary/tests/cli_diagnostics.rs
//! First-use refusals must be actionable and must precede every mutation.

use std::process::Command;

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_conary"))
        .args(args)
        .env("NO_COLOR", "1")
        .env_remove("RUST_LOG")
        .output()
        .unwrap()
}

#[test]
fn install_refusal_has_facts_and_next_steps_before_database_creation() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("absent.db");
    let output = run(&["install", "fixture", "--db-path", db.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(
        stderr,
        concat!(
            "error: Confirmation is required before applying changes.\n",
            "  Command: conary install\n",
            "  Impact: May change packages, files, scriptlets, ownership, or the live Conary database.\n",
            "  Root: Current --root or similar arguments are not sufficient isolation for this command yet.\n",
            "note: Use --dry-run when available to preview first.\n",
            "note: Rerun this command with --yes when you intend to apply it.\n",
        )
    );
    assert!(!db.exists());
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
}

#[test]
fn missing_custom_database_names_the_path_without_debug_quotes() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("database with spaces.db");
    let output = run(&["list", "--db-path", db.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        format!(
            "error: Custom database not initialized.\n  Database: {}\nnote: Run 'conary system init --db-path <PATH>' with the same custom path.\n",
            db.display()
        )
    );
    assert!(!db.exists());
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
}

#[test]
fn terminal_database_error_preserves_the_same_facts_with_and_without_color() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("database with spaces.db");
    for no_color in [false, true] {
        let mut command = Command::new("script");
        command
            .args([
                "-qec",
                "exec \"$CONARY_DIAGNOSTIC_EXE\" list --db-path \"$CONARY_DIAGNOSTIC_DB\"",
                "/dev/null",
            ])
            .env("CONARY_DIAGNOSTIC_EXE", env!("CARGO_BIN_EXE_conary"))
            .env("CONARY_DIAGNOSTIC_DB", &db)
            .env("TERM", "xterm")
            .env_remove("RUST_LOG")
            .env_remove("NO_COLOR")
            .env_remove("CLICOLOR_FORCE");
        if no_color {
            command.env("NO_COLOR", "1");
        }
        let output = command
            .output()
            .expect("PTY capture requires util-linux script");
        assert_eq!(output.status.code(), Some(1));
        let text = String::from_utf8(output.stdout).unwrap();
        assert_eq!(text.contains('\x1b'), !no_color, "{text:?}");
        let plain = console::strip_ansi_codes(&text).replace("\r\n", "\n");
        assert_eq!(
            plain,
            format!(
                "error: Custom database not initialized.\n  Database: {}\nnote: Run 'conary system init --db-path <PATH>' with the same custom path.\n",
                db.display()
            )
        );
        assert!(output.stderr.is_empty());
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    }
}

#[path = "cli_diagnostics/verification.rs"]
mod verification;

#[path = "cli_diagnostics/machine.rs"]
mod machine;
