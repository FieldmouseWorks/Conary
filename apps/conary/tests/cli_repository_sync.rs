// apps/conary/tests/cli_repository_sync.rs

#![cfg(test)]
//! Real metadata synchronization, retained partial results, and executable recovery.

#[path = "cli_repository_sync/fixture.rs"]
mod fixture;
use fixture::*;

#[test]
fn mixed_sync_preserves_cached_failure_and_retries_only_failed_sources() {
    for mode in MODES {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("database ' $(exit 21) `exit 22`.db");
        conary_core::db::init(&db).unwrap();
        let server = Server::new();
        let bad = "bad ' $(exit 23) `exit 24` ; source";
        for (name, route) in [(bad, "/bad"), ("good", "/good")] {
            server.metadata(route, "1.0");
            add(&db, &server, name, route);
        }
        success(&db, &["repo", "sync", "--force"]);
        stale(&db, bad);
        stale(&db, "good");
        let before = snapshot(&db, bad);
        server.fail("/bad", 404);
        server.metadata("/good", "2.0");
        server.take_requests();
        let failed = run(mode, &db, &["repo", "sync", "--force"]);
        assert_ne!(failed.code, 0, "{}", failed.text);
        assert!(
            failed.text.contains("Repository synchronization:"),
            "{}",
            failed.text
        );
        assert!(failed.text.contains(&format!("Database: {}", db.display())));
        assert_eq!(failed.rows("[fail]", bad), 1, "{}", failed.text);
        assert_eq!(failed.rows("[ok]", "good"), 1, "{}", failed.text);
        assert_eq!(failed.text.matches("HTTP status: 404").count(), 1);
        assert!(
            failed
                .text
                .contains(&format!("Metadata URL: {}/bad/metadata.json", server.url))
        );
        assert!(failed.text.contains("Package records synchronized: 1"));
        if !mode.tty {
            assert!(failed.stdout.contains("[ok]") && failed.stdout.contains("[fail]"));
            assert!(!failed.stdout.contains("HTTP status"));
            assert!(
                failed
                    .stderr
                    .starts_with("error: Repository metadata synchronization failed.")
            );
        }
        assert_eq!(
            snapshot(&db, bad),
            before,
            "failed refresh changed stored metadata"
        );
        assert_eq!(snapshot(&db, "good").2, ["2.0"]);
        assert_ne!(snapshot(&db, "good").0, before.0);
        let requests = server.take_requests();
        assert_eq!(requests.len(), 2, "{requests:?}");
        server.metadata("/bad", "3.0");
        let actions: Vec<_> = failed
            .text
            .lines()
            .filter_map(|line| line.strip_prefix("note: Run: "))
            .collect();
        assert_eq!(actions.len(), 1);
        let database_arg = format!("--db-path={}", db.display());
        let recovered = execute_recovery(
            actions[0],
            &["repo", "sync", "--force", &database_arg, "--", bad],
        );
        assert_eq!(recovered.rows("[ok]", bad), 1);
        assert!(recovered.stderr.is_empty());
        assert_eq!(snapshot(&db, bad).2, ["3.0"]);
        assert_eq!(server.take_requests(), ["/bad/metadata.json"]);
    }
}

#[test]
fn multiple_failures_preserve_source_identity_and_option_like_retry_names() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("sync.db");
    conary_core::db::init(&db).unwrap();
    let server = Server::new();
    for (name, route) in [("first", "/first"), ("second", "/second")] {
        server.metadata(route, "1");
        add(&db, &server, name, route);
    }
    let conn = conary_core::db::open(&db).unwrap();
    let mut second = conary_core::db::models::Repository::find_by_name(&conn, "second")
        .unwrap()
        .unwrap();
    second.name = "--second ' source".into();
    second.update(&conn).unwrap();
    server.fail("/first", 404);
    server.fail("/second", 403);
    for mode in MODES {
        let capture = run(mode, &db, &["repo", "sync", "--force"]);
        assert_ne!(capture.code, 0);
        for expected in [
            "Repository: first",
            "Repository: --second ' source",
            "HTTP status: 404",
            "HTTP status: 403",
        ] {
            assert_eq!(
                capture.text.matches(expected).count(),
                1,
                "{}",
                capture.text
            );
        }
        let actions: Vec<_> = capture
            .text
            .lines()
            .filter_map(|line| line.strip_prefix("note: Run: "))
            .collect();
        assert_eq!(actions.len(), 2);
        server.metadata("/first", "2");
        server.metadata("/second", "2");
        let database_arg = format!("--db-path={}", db.display());
        for action in actions {
            let name = if action.contains("--second") {
                "--second ' source"
            } else {
                "first"
            };
            execute_recovery(
                action,
                &["repo", "sync", "--force", &database_arg, "--", name],
            );
        }
        server.fail("/first", 404);
        server.fail("/second", 403);
    }
}

#[test]
fn fresh_and_disabled_sources_do_not_fetch_and_unknown_name_has_same_database_guidance() {
    for mode in MODES {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("sync ' db.db");
        conary_core::db::init(&db).unwrap();
        let server = Server::new();
        server.metadata("/source", "1");
        add(&db, &server, "source", "/source");
        let synced = run(mode, &db, &["repo", "sync", "--force"]);
        assert_eq!(synced.code, 0, "{}", synced.text);
        assert_eq!(synced.rows("[ok]", "source"), 1, "{}", synced.text);
        server.take_requests();
        let current = run(mode, &db, &["repo", "sync"]);
        assert_eq!(current.code, 0);
        assert!(
            current
                .text
                .contains("No repository metadata checks are due."),
            "{}",
            current.text
        );
        assert!(server.take_requests().is_empty());
        success(&db, &["repo", "disable", "source"]);
        let disabled = run(mode, &db, &["repo", "sync", "--force"]);
        assert_eq!(disabled.code, 0);
        assert!(disabled.text.contains("No enabled repositories to sync."));
        assert!(
            disabled
                .text
                .contains("All configured repositories are disabled.")
        );
        assert!(server.take_requests().is_empty());
        // An explicit source continues to select a disabled repository.
        let named = run(mode, &db, &["repo", "sync", "source", "--force"]);
        assert_eq!(named.code, 0);
        assert_eq!(server.take_requests(), ["/source/metadata.json"]);
        let unknown = run(mode, &db, &["repo", "sync", "unknown"]);
        assert_ne!(unknown.code, 0);
        assert!(unknown.text.contains("Repository: unknown"));
        assert!(
            unknown
                .text
                .contains(&format!("Database: {}", db.display()))
        );
        let action = unknown
            .text
            .lines()
            .find_map(|line| line.strip_prefix("note: Run: "))
            .unwrap();
        let database_arg = format!("--db-path={}", db.display());
        let listed = execute_recovery(action, &["repo", "list", "--all", &database_arg]);
        assert_eq!(listed.rows("[off]", "source"), 1);
    }
}

#[test]
fn sync_control_characters_are_visible_data_and_recovery_uses_same_value_instructions() {
    for mode in MODES {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("line\nbreak.db");
        conary_core::db::init(&db).unwrap();
        let server = Server::new();
        server.metadata("/source", "1");
        let name = "line\n[ok] injected\x1b[2J";
        add(&db, &server, name, "/source");
        server.fail("/source", 404);
        let capture = run(mode, &db, &["repo", "sync", "--force"]);
        assert_ne!(capture.code, 0);
        assert!(!capture.text.contains(name) && !capture.text.contains(db.to_str().unwrap()));
        assert!(
            !capture
                .text
                .lines()
                .any(|line| line.starts_with("[ok] injected"))
        );
        assert!(capture.text.contains("line\\n[ok] injected\\u{1b}[2J"));
        assert!(
            capture
                .text
                .contains("Use 'conary repo sync --force' with repository name")
        );
        assert!(capture.text.contains("and the same database path."));
        assert!(!capture.text.contains("note: Run:"));
    }
}
