// apps/conary-test/src/config/tests/native_corpus/typed_trigger.rs
#![cfg(test)]

use super::super::{load_manifest, remi_manifest_path};
use crate::config::manifest::{Assertion, JsonExpectation, TestDef, TestManifest, TestStep};
use crate::engine::{assertions::evaluate_assertion, variables::expand_assertion};
use rusqlite::Connection;
use serde_json::{Value, json};

const COMMAND: &str = "sqlite3 -json ${DB_PATH} \"SELECT t.name AS name, ct.status AS status, ct.matched_files AS matched_files FROM changeset_triggers ct JOIN triggers t ON t.id = ct.trigger_id WHERE t.name = 'phase4-corpus-trigger' ORDER BY ct.id DESC LIMIT 1\"";
const QUERY: &str = "SELECT t.name AS name, ct.status AS status, ct.matched_files AS matched_files FROM changeset_triggers ct JOIN triggers t ON t.id = ct.trigger_id WHERE t.name = 'phase4-corpus-trigger' ORDER BY ct.id DESC LIMIT 1";
const EXPECTED: &str = "phase4-corpus-trigger|completed|1";

#[test]
fn native_corpus_tnpm15_trigger_requires_exact_latest_named_result() {
    let manifest = load_manifest(&remi_manifest_path(
        "phase4-native-daily-driver-corpus.toml",
    ))
    .expect("load native daily-driver corpus manifest");
    let test = get_test(&manifest, "TNPM15");
    assert_eq!(test.step.len(), 10, "TNPM15 must retain its ten-step order");

    let step = &test.step[9];
    assert_eq!(step.run.as_deref(), Some(COMMAND));
    assert_eq!(COMMAND.matches("sqlite3").count(), 1);
    assert_eq!(COMMAND.matches("SELECT").count(), 1);
    assert!(!QUERY.contains(';'));
    assert_eq!(sqlite_query(step), QUERY);
    let assertion = typed_root_assertion(step);
    let expected = json!([{"name":"phase4-corpus-trigger","status":"completed","matched_files":1}]);

    for distro in ["fedora44", "ubuntu-26.04", "arch"] {
        let overrides = manifest
            .distro_overrides
            .get(distro)
            .unwrap_or_else(|| panic!("missing {distro} overrides"));
        assert_eq!(
            crate::engine::variables::expand_variables(COMMAND, overrides),
            COMMAND,
            "{distro} trigger command"
        );
        let expanded = expand_assertion(assertion, overrides);
        assert_eq!(
            &expanded.stdout_json.as_ref().unwrap()[0].expected,
            &JsonExpectation::Equals(expected.clone()),
            "{distro} trigger result"
        );

        let database = trigger_database();
        let actual = execute_query(&database, QUERY);
        assert_eq!(actual, expected, "{distro}: latest named result and scope");
        assert!(evaluate_assertion(&expanded, 0, &actual.to_string(), "").is_ok());
        reject_invalid_results(&expanded, &expected, distro);

        demonstrate_legacy_false_positive(&database, &expanded, &expected);
    }
}

fn get_test<'a>(manifest: &'a TestManifest, id: &str) -> &'a TestDef {
    manifest
        .test
        .iter()
        .find(|test| test.id == id)
        .unwrap_or_else(|| panic!("missing {id}"))
}

fn sqlite_query(step: &TestStep) -> &str {
    step.run
        .as_deref()
        .and_then(|command| command.strip_prefix("sqlite3 -json ${DB_PATH} \""))
        .and_then(|query| query.strip_suffix('"'))
        .expect("TNPM15 trigger query must be one quoted sqlite3 JSON command")
}

fn typed_root_assertion(step: &TestStep) -> &Assertion {
    let assertion = step.assert.as_ref().expect("trigger step assertion");
    assert_eq!(assertion.exit_code, Some(0));
    assert!(assertion.exit_code_not.is_none());
    assert!(assertion.stdout_contains.is_none());
    assert!(assertion.stdout_not_contains.is_none());
    assert!(assertion.stdout_contains_all.is_none());
    assert!(assertion.stdout_contains_any.is_none());
    assert!(assertion.stdout_contains_if_success.is_none());
    assert!(assertion.stdout_contains_any_if_success.is_none());
    assert!(assertion.stderr_contains.is_none());
    assert!(assertion.stderr_not_contains.is_none());
    let checks = assertion
        .stdout_json
        .as_ref()
        .expect("exact JSON trigger result");
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].pointer, "", "compare the whole result array");
    assert_eq!(
        checks[0].expected,
        JsonExpectation::Equals(json!([{
            "name": "phase4-corpus-trigger",
            "status": "completed",
            "matched_files": 1
        }]))
    );
    assertion
}

fn trigger_database() -> Connection {
    let database = Connection::open_in_memory().expect("open in-memory SQLite database");
    database
        .execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE triggers (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 name TEXT NOT NULL,
                 pattern TEXT NOT NULL,
                 handler TEXT NOT NULL
             );
             CREATE TABLE changesets (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 description TEXT
             );
             CREATE TABLE changeset_triggers (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 changeset_id INTEGER NOT NULL REFERENCES changesets(id) ON DELETE CASCADE,
                 trigger_id INTEGER NOT NULL REFERENCES triggers(id) ON DELETE CASCADE,
                 status TEXT NOT NULL DEFAULT 'pending'
                     CHECK(status IN ('pending', 'running', 'completed', 'failed', 'skipped')),
                 matched_files INTEGER NOT NULL DEFAULT 0,
                 started_at TEXT,
                 completed_at TEXT,
                 output TEXT,
                 UNIQUE(changeset_id, trigger_id)
             );
             INSERT INTO triggers VALUES
                 (1, 'phase4-corpus-trigger', '/usr/lib/systemd/system/phase4-corpus.service', '/usr/bin/true'),
                 (2, 'unrelated-trigger', '/usr/share/unrelated/*', '/bin/true');
             INSERT INTO changesets VALUES (1, 'older matching result'), (2, 'latest matching result'), (3, 'unrelated result');
             INSERT INTO changeset_triggers (id, changeset_id, trigger_id, status, matched_files) VALUES
                 (1, 1, 1, 'pending', 0),
                 (2, 2, 1, 'completed', 1),
                 (3, 3, 2, 'completed', 10);",
        )
        .expect("create schema with persisted trigger constraints");
    database
}

fn execute_query(database: &Connection, query: &str) -> Value {
    let mut statement = database.prepare(query).expect("prepare manifest SQL");
    let rows = statement
        .query_map([], |row| {
            Ok(json!({
                "name": row.get::<_, String>(0)?,
                "status": row.get::<_, String>(1)?,
                "matched_files": row.get::<_, i64>(2)?,
            }))
        })
        .expect("execute manifest SQL")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("read manifest SQL rows");
    Value::Array(rows)
}

fn reject_invalid_results(assertion: &Assertion, expected: &Value, distro: &str) {
    for (stdout, defect) in [
        ("[]", "absent matching result"),
        (
            r#"[{"name":"other-trigger","status":"completed","matched_files":1}]"#,
            "wrong name",
        ),
        (
            r#"[{"name":"phase4-corpus-trigger","status":"failed","matched_files":1}]"#,
            "wrong status",
        ),
        (
            r#"[{"name":"phase4-corpus-trigger","status":"completed","matched_files":0}]"#,
            "wrong count",
        ),
        (
            r#"[{"name":"phase4-corpus-trigger","status":"completed","matched_files":10}]"#,
            "count accepted by old substring",
        ),
        (
            r#"[{"name":"phase4-corpus-trigger","status":"completed","matched_files":"1"}]"#,
            "wrong count type",
        ),
        (
            r#"[{"name":"phase4-corpus-trigger","status":"completed","matched_files":1.0}]"#,
            "decimal count type",
        ),
        (
            r#"[{"name":"phase4-corpus-trigger","status":"completed"}]"#,
            "missing field",
        ),
        (
            r#"[{"name":"phase4-corpus-trigger","status":"completed","matched_files":1,"extra":true}]"#,
            "extra field",
        ),
        (
            r#"[{"name":"phase4-corpus-trigger","status":"completed","matched_files":1},{"name":"phase4-corpus-trigger","status":"completed","matched_files":1}]"#,
            "extra output row",
        ),
        ("not JSON", "malformed JSON"),
        (
            r#"[{"name":"phase4-corpus-trigger","status":"completed","matched_files":1}] trailing"#,
            "trailing JSON data",
        ),
    ] {
        assert!(
            evaluate_assertion(assertion, 0, stdout, "").is_err(),
            "{distro} must reject {defect}: {stdout}"
        );
    }
    assert!(
        evaluate_assertion(assertion, 1, &expected.to_string(), "").is_err(),
        "{distro} must reject a nonzero exit"
    );
}

fn demonstrate_legacy_false_positive(
    database: &Connection,
    assertion: &Assertion,
    expected: &Value,
) {
    database
        .execute(
            "UPDATE changeset_triggers SET matched_files = 10 WHERE id = 2",
            [],
        )
        .expect("create DB-realizable latest result with ten matched files");
    let legacy_row = database
        .query_row(
            "SELECT t.name || '|' || ct.status || '|' || ct.matched_files FROM changeset_triggers ct JOIN triggers t ON t.id = ct.trigger_id WHERE t.name = 'phase4-corpus-trigger' ORDER BY ct.id DESC LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("render old latest matching trigger row");
    assert_eq!(legacy_row, "phase4-corpus-trigger|completed|10");
    let old_assertion = Assertion {
        exit_code: Some(0),
        stdout_contains: Some(EXPECTED.into()),
        ..Assertion::default()
    };
    assert!(evaluate_assertion(&old_assertion, 0, &legacy_row, "").is_ok());

    let latest = execute_query(database, QUERY);
    assert_eq!(latest[0]["matched_files"], json!(10));
    assert!(evaluate_assertion(assertion, 0, &latest.to_string(), "").is_err());
    assert_ne!(&latest, expected);

    database
        .execute("DELETE FROM changeset_triggers WHERE trigger_id = 1", [])
        .expect("remove target trigger result for absent-result proof");
    let absent = execute_query(database, QUERY);
    assert_eq!(absent, json!([]));
    assert!(evaluate_assertion(assertion, 0, &absent.to_string(), "").is_err());
}
