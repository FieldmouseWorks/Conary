// apps/conary/tests/cli_repository_enrollment.rs

#![cfg(test)]
//! Enrollment and state-command output never treats source identities as terminal text.

use conary_core::db::models::Repository;
use std::path::Path;
use std::process::Command;

#[derive(Clone, Copy, Debug)]
struct Mode {
    tty: bool,
    no_color: bool,
}

const MODES: [Mode; 4] = [
    Mode {
        tty: false,
        no_color: false,
    },
    Mode {
        tty: false,
        no_color: true,
    },
    Mode {
        tty: true,
        no_color: false,
    },
    Mode {
        tty: true,
        no_color: true,
    },
];

struct Capture {
    code: i32,
    text: String,
    stdout: String,
    stderr: String,
}

fn visible(value: &str) -> String {
    value
        .chars()
        .flat_map(|c| {
            if c.is_control() {
                c.escape_debug().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

fn run(mode: Mode, args: &[&str]) -> Capture {
    let mut command = if mode.tty {
        let arguments = (0..args.len())
            .map(|i| format!("\"$CONARY_ENROLL_ARG_{i}\""))
            .collect::<Vec<_>>()
            .join(" ");
        let mut command = Command::new("script");
        command
            .args([
                "-qec",
                &format!("exec \"$CONARY_ENROLL_EXE\" {arguments}"),
                "/dev/null",
            ])
            .env("CONARY_ENROLL_EXE", env!("CARGO_BIN_EXE_conary"));
        for (i, arg) in args.iter().enumerate() {
            command.env(format!("CONARY_ENROLL_ARG_{i}"), arg);
        }
        command
    } else {
        let mut command = Command::new(env!("CARGO_BIN_EXE_conary"));
        command.args(args);
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
    if mode.no_color {
        command.env("NO_COLOR", "1");
    }
    let output = command
        .output()
        .expect("PTY capture requires util-linux script");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    let raw = format!("{stdout}{stderr}");
    assert_eq!(
        raw.contains('\x1b'),
        mode.tty && !mode.no_color,
        "{mode:?}: {raw:?}"
    );
    assert!(
        !raw.contains("\x1b[2J"),
        "source injected a terminal clear: {raw:?}"
    );
    if mode.tty {
        assert!(stderr.is_empty());
    }
    Capture {
        code: output.status.code().unwrap(),
        text: console::strip_ansi_codes(&raw).replace("\r\n", "\n"),
        stdout,
        stderr,
    }
}

fn enroll(mode: Mode, db: &Path, name: &str, extra: &[&str]) -> Capture {
    let mut args = vec![
        "repo",
        "add",
        "--db-path",
        db.to_str().unwrap(),
        "--package-format",
        "json",
        "--yes",
    ];
    args.extend_from_slice(extra);
    args.extend(["--", name, "https://example.invalid/metadata"]);
    run(mode, &args)
}

fn state(mode: Mode, db: &Path, name: &str, operation: &str) -> Capture {
    run(
        mode,
        &[
            "repo",
            operation,
            "--db-path",
            db.to_str().unwrap(),
            "--",
            name,
        ],
    )
}

fn find(db: &Path, name: &str) -> Option<Repository> {
    Repository::find_by_name(&conary_core::db::open(db).unwrap(), name).unwrap()
}

fn assert_success(capture: &Capture, mode: Mode, db: &Path, name: &str, heading: &str) {
    assert_eq!(capture.code, 0, "{}", capture.text);
    assert!(capture.text.starts_with(heading), "{}", capture.text);
    assert!(
        capture
            .text
            .contains(&format!("Database: {}", visible(db.to_str().unwrap())))
    );
    assert_eq!(
        capture
            .text
            .lines()
            .filter(|line| line
                .strip_prefix("[ok]")
                .is_some_and(|value| value.trim_start() == visible(name)))
            .count(),
        1,
        "{}",
        capture.text
    );
    if !mode.tty {
        assert!(capture.stderr.is_empty());
    }
}

fn inspect_action(capture: &Capture, db: &Path) {
    let action = capture
        .text
        .lines()
        .find_map(|line| line.strip_prefix("note: Run: "))
        .unwrap();
    assert!(action.starts_with("conary "));
    let decoded = Command::new("sh")
        .args([
            "-c",
            &format!("conary() {{ printf '%s\\0' \"$@\"; }}; {action}"),
        ])
        .output()
        .unwrap();
    assert!(decoded.status.success() && decoded.stderr.is_empty());
    let args: Vec<_> = decoded
        .stdout
        .strip_suffix(&[0])
        .unwrap()
        .split(|c| *c == 0)
        .map(|s| std::str::from_utf8(s).unwrap())
        .collect();
    assert_eq!(
        args,
        [
            "repo",
            "list",
            "--all",
            &format!("--db-path={}", db.display())
        ]
    );
    let action = action.replacen("conary", "\"$CONARY_ENROLL_EXE\"", 1);
    let output = Command::new("sh")
        .args(["-c", &action])
        .env("CONARY_ENROLL_EXE", env!("CARGO_BIN_EXE_conary"))
        .env("NO_COLOR", "1")
        .env_remove("RUST_LOG")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn enrollment_enable_disable_and_remove_preserve_exact_identity_and_state() {
    for mode in MODES {
        for name in [
            "source ' $(exit 23) `exit 24`",
            "--option-like",
            "line\n[ok] forged\x1b[2J",
        ] {
            let temp = tempfile::tempdir().unwrap();
            let db = temp.path().join("database ' $(exit 25).db");
            conary_core::db::init(&db).unwrap();
            let content = "https://example.invalid/reference\n[warn] forged\x1b[2J";
            let added = enroll(
                mode,
                &db,
                name,
                &["--content-url", content, "--priority", "17", "--disabled"],
            );
            assert_success(&added, mode, &db, name, "Repository added:");
            assert!(
                added
                    .text
                    .contains("Metadata URL: https://example.invalid/metadata")
            );
            assert!(added.text.contains(&format!(
                "Content URL: {} (reference mirror)",
                visible(content)
            )));
            assert!(added.text.contains("Enabled: false") && added.text.contains("Priority: 17"));
            assert!(
                added
                    .text
                    .contains("Repository trust: typed JSON/Remi authority")
            );
            assert!(added.text.contains("Security advisories: unknown"));
            let repo = find(&db, name).unwrap();
            assert!(!repo.enabled);
            assert_eq!(repo.priority, 17);
            assert_eq!(repo.content_url.as_deref(), Some(content));
            let enabled = state(mode, &db, name, "enable");
            assert_success(&enabled, mode, &db, name, "Repository enabled:");
            assert!(find(&db, name).unwrap().enabled);
            let disabled = state(mode, &db, name, "disable");
            assert_success(&disabled, mode, &db, name, "Repository disabled:");
            assert!(!find(&db, name).unwrap().enabled);
            let removed = state(mode, &db, name, "remove");
            assert_success(&removed, mode, &db, name, "Repository removed:");
            assert!(find(&db, name).is_none());
        }
    }
}

#[test]
fn failed_repository_commands_escape_causes_preserve_state_and_offer_safe_inspection() {
    for mode in MODES {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("selected ' db.db");
        conary_core::db::init(&db).unwrap();
        let name = "line\n[ok] forged\x1b[2J";
        let added = enroll(mode, &db, name, &["--priority", "17"]);
        assert_eq!(added.code, 0, "{}", added.text);
        let original = find(&db, name).unwrap();
        let duplicate = enroll(mode, &db, name, &["--priority", "99"]);
        assert_ne!(duplicate.code, 0);
        assert!(
            duplicate
                .text
                .starts_with("error: Repository enrollment failed."),
            "{}",
            duplicate.text
        );
        assert!(
            duplicate
                .text
                .contains(&format!("Repository: {}", visible(name)))
        );
        assert!(duplicate.text.contains("Cause: Conflict:"));
        assert!(!duplicate.text.contains(name));
        if !mode.tty {
            assert!(duplicate.stdout.is_empty());
        }
        inspect_action(&duplicate, &db);
        let persisted = find(&db, name).unwrap();
        assert_eq!(persisted.id, original.id);
        assert_eq!(persisted.priority, 17);
        assert_eq!(persisted.url, original.url);
        for operation in ["enable", "disable", "remove"] {
            let unknown = "missing\n[ok] forged\x1b[2J";
            let failed = state(mode, &db, unknown, operation);
            assert_ne!(failed.code, 0);
            assert!(
                failed
                    .text
                    .contains(&format!("Repository: {}", visible(unknown))),
                "{}",
                failed.text
            );
            assert!(failed.text.contains(&format!("Database: {}", db.display())));
            assert!(failed.text.contains("Cause:") && failed.text.contains("not found"));
            assert!(!failed.text.contains(unknown));
            if !mode.tty {
                assert!(failed.stdout.is_empty());
            }
            inspect_action(&failed, &db);
            assert!(find(&db, unknown).is_none());
            assert_eq!(find(&db, name).unwrap().id, original.id);
        }
    }
}

#[test]
fn control_database_paths_get_same_path_instructions_without_runnable_placeholders() {
    for mode in MODES {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("line\n[ok] forged.db");
        conary_core::db::init(&db).unwrap();
        let added = enroll(mode, &db, "source", &[]);
        assert_success(&added, mode, &db, "source", "Repository added:");
        let failed = state(mode, &db, "unknown", "enable");
        assert_ne!(failed.code, 0);
        assert!(
            failed
                .text
                .contains(&format!("Database: {}", visible(db.to_str().unwrap())))
        );
        assert!(!failed.text.contains(db.to_str().unwrap()));
        assert!(failed.text.contains(
            "Use 'conary repo list --all' with --db-path set to the same database path."
        ));
        assert!(!failed.text.contains("note: Run:"));
        assert!(find(&db, "source").unwrap().enabled);
    }
}

#[test]
fn static_enrollment_and_trust_reset_render_the_exact_source_and_preserve_trust_gates() {
    for mode in MODES {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("static ' db.db");
        conary_core::db::init(&db).unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(source.join("metadata")).unwrap();
        let key = conary_core::ccs::signing::SigningKeyPair::generate();
        let root = conary_core::trust::ceremony::create_initial_root_single_key(&key, 365).unwrap();
        let fingerprint = root.signed.roles["root"].keyids[0].clone();
        let identity: conary_core::repository::static_repo::RepoIdentity = serde_json::from_value(serde_json::json!({
            "schema": conary_core::repository::static_repo::SCHEMA_VERSION,
            "repo":{"name":"signed-fixture", "description":"fixture description\nRoot key IDs: forged"},
            "trust":{"root_key_ids":[fingerprint]}
        })).unwrap();
        std::fs::write(
            source.join("conary-repo.toml"),
            toml::to_string(&identity).unwrap(),
        )
        .unwrap();
        std::fs::write(
            source.join("metadata/root.json"),
            serde_json::to_vec(&root).unwrap(),
        )
        .unwrap();
        let name = "static\n[ok] forged\x1b[2J";
        let args = [
            "repo",
            "add",
            "--db-path",
            db.to_str().unwrap(),
            "--fingerprint",
            &fingerprint,
            "--",
            name,
            source.to_str().unwrap(),
        ];
        let added = run(mode, &args);
        assert_success(&added, mode, &db, name, "Repository added:");
        assert!(
            added.text.contains("TUF metadata URL:")
                && added.text.contains("Default strategy: static")
        );
        assert!(!added.text.contains("JSON/Remi authority"));
        let repo = find(&db, name).unwrap();
        assert!(repo.tuf_enabled && repo.enabled);
        let conn = conary_core::db::open(&db).unwrap();
        let roots = || {
            conn.query_row("SELECT count(*) FROM tuf_roots", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap()
        };
        assert_eq!(roots(), 1);
        let reset = state(mode, &db, name, "reset-trust");
        assert_eq!(reset.code, 0, "{}", reset.text);
        assert!(reset.text.starts_with("Repository trust reset:"));
        assert!(reset.text.contains(&format!("Database: {}", db.display())));
        assert!(reset.text.contains(&visible(name)) && !reset.text.contains(name));
        assert!(
            reset
                .text
                .contains("root-key fingerprints verified out of band")
        );
        assert!(!reset.text.contains("note: Run:"));
        assert_eq!(roots(), 0);
        let disabled = find(&db, name).unwrap();
        assert!(!disabled.enabled && !disabled.tuf_enabled);
        // Reset does not silently establish trust on a repeated enrollment.
        let duplicate = run(mode, &args);
        assert_ne!(duplicate.code, 0);
        inspect_action(&duplicate, &db);
        let missing = state(mode, &db, "missing static source", "reset-trust");
        assert_ne!(missing.code, 0);
        assert!(missing.text.contains("Repository: missing static source"));
        inspect_action(&missing, &db);
        assert_eq!(roots(), 0);
        assert!(!find(&db, name).unwrap().enabled);
    }
}
