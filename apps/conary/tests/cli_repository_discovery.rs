// apps/conary/tests/cli_repository_discovery.rs
//! Disposable repository discovery journey and terminal/pipe presentation proof.

use conary_core::db::models::Repository;
use std::path::Path;
use std::process::Command;

fn run(db: &Path, args: &[&str]) -> (String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_conary"))
        .args(args)
        .arg("--db-path")
        .arg(db)
        .env("NO_COLOR", "1")
        .env_remove("RUST_LOG")
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(output.status.success(), "{args:?}: {stdout}\n{stderr}");
    assert!(!stdout.contains('\x1b') && !stderr.contains('\x1b'));
    (stdout, stderr)
}

fn write_metadata(path: &Path, packages: bool) {
    std::fs::write(
        path.join("metadata.json"),
        serde_json::json!({
            "name": "fixture", "version": "1",
            "packages": if packages { vec![serde_json::json!({
                "name": "fixture-package", "version": "1.2", "release": "3",
                "version_scheme": "rpm", "description": "a searchable needle",
                "checksum": "a".repeat(64), "size": 17,
                "download_url": "https://example.invalid/fixture.rpm"
            })] } else { vec![] }
        })
        .to_string(),
    )
    .unwrap();
}

fn add(db: &Path, directory: &Path) {
    run(
        db,
        &[
            "repo",
            "add",
            "fixture",
            directory.to_str().unwrap(),
            "--package-format",
            "json",
            "--yes",
        ],
    );
}

#[test]
fn discovery_journey_distinguishes_missing_disabled_unpublished_and_cached_sources() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("database with ' quotes.db");
    conary_core::db::init(&db).unwrap();
    let (stdout, stderr) = run(&db, &["search", "needle"]);
    assert!(stdout.contains("No matching packages in cached metadata from enabled repositories."));
    assert!(stderr.contains("conary repo add --help"));
    assert!(!stderr.contains("repo sync"));

    write_metadata(temp.path(), true);
    add(&db, temp.path());
    let (_, stderr) = run(&db, &["query", "repquery"]);
    assert!(stderr.contains("Repository fixture has no published metadata."));
    assert!(stderr.contains("conary repo sync --force --db-path='"));
    assert!(stderr.contains("'\"'\"'"));

    // Execute the exact printed recovery command, including its quoted database.
    let recovery = stderr
        .lines()
        .find_map(|line| line.strip_prefix("note: Run: "))
        .unwrap();
    let executable = env!("CARGO_BIN_EXE_conary");
    let command = recovery.replacen("conary", "\"$CONARY_DISCOVERY_EXE\"", 1);
    let output = Command::new("sh")
        .args(["-c", &command])
        .env("CONARY_DISCOVERY_EXE", executable)
        .env("NO_COLOR", "1")
        .env_remove("RUST_LOG")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let (search, stderr) = run(&db, &["search", "needle"]);
    assert!(stderr.is_empty(), "{stderr}");
    for fact in [
        "Version: 1.2",
        "Release: 3",
        "Architecture: Unspecified",
        "Repository: fixture",
        "Packages: 1",
    ] {
        assert!(search.contains(fact), "{search}");
    }
    let (query, _) = run(&db, &["query", "repquery", "needle"]);
    assert_eq!(search, query);
    let (_, stderr) = run(&db, &["search", "absent"]);
    assert!(
        stderr.is_empty(),
        "A checked empty match must not prescribe a sync: {stderr}"
    );

    let conn = conary_core::db::open(&db).unwrap();
    let mut repo = Repository::find_by_name(&conn, "fixture").unwrap().unwrap();
    repo.last_checked_at = Some("2000-01-01T00:00:00Z".into());
    repo.update(&conn).unwrap();
    let (stdout, stderr) = run(&db, &["search", "needle"]);
    assert!(stdout.contains("Packages: 1"));
    assert!(stderr.contains("cached results may be outdated"));
    assert!(stderr.contains("conary repo sync --db-path="));

    run(&db, &["repo", "disable", "fixture"]);
    for args in [
        vec!["search", "needle"],
        vec!["query", "repquery", "needle"],
        vec!["query", "repquery"],
    ] {
        let (stdout, stderr) = run(&db, &args);
        assert!(!stdout.contains("fixture-package"));
        assert!(stderr.contains("All configured repositories are disabled."));
        assert!(stderr.contains("conary repo enable <NAME> --db-path="));
        assert!(!stderr.contains("repo sync"));
    }
    let (stdout, _) = run(&db, &["repo", "list"]);
    assert!(stdout.contains("No enabled repositories."));
    assert!(!stdout.contains("No repositories configured"));
    let (stdout, _) = run(&db, &["repo", "list", "--all"]);
    assert!(stdout.contains("[off]"));
    assert!(stdout.contains("Last published:"));
    run(&db, &["repo", "enable", "fixture"]);
    write_metadata(temp.path(), false);
    run(&db, &["repo", "sync", "--force"]);
    let (stdout, stderr) = run(&db, &["query", "repquery"]);
    assert!(stdout.contains("Packages: 0"));
    assert!(
        stderr.is_empty(),
        "A published empty catalog is valid: {stderr}"
    );
}

#[test]
fn discovery_terminal_frames_match_pipe_facts_with_and_without_color() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("fixture.db");
    conary_core::db::init(&db).unwrap();
    write_metadata(temp.path(), true);
    add(&db, temp.path());
    for args in [
        vec!["repo", "list"],
        vec!["search", "needle"],
        vec!["query", "repquery"],
    ] {
        let (stdout, stderr) = run(&db, &args);
        for no_color in [false, true] {
            let command = format!(
                "exec \"$CONARY_DISCOVERY_EXE\" {} --db-path \"$CONARY_DISCOVERY_DB\"",
                args.join(" ")
            );
            let mut capture = Command::new("script");
            capture
                .args(["-qec", &command, "/dev/null"])
                .env("CONARY_DISCOVERY_EXE", env!("CARGO_BIN_EXE_conary"))
                .env("CONARY_DISCOVERY_DB", &db)
                .env("TERM", "xterm")
                .env_remove("NO_COLOR")
                .env_remove("CLICOLOR_FORCE")
                .env_remove("RUST_LOG");
            if no_color {
                capture.env("NO_COLOR", "1");
            }
            let output = capture.output().unwrap();
            assert!(output.status.success());
            let text = String::from_utf8(output.stdout).unwrap();
            assert_eq!(text.contains('\x1b'), !no_color, "{text:?}");
            assert_eq!(
                console::strip_ansi_codes(&text).replace("\r\n", "\n"),
                format!("{stdout}{stderr}")
            );
        }
    }
}
