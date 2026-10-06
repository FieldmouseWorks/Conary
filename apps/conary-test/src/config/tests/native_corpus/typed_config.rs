// apps/conary-test/src/config/tests/native_corpus/typed_config.rs
#![cfg(test)]

use super::super::{load_manifest, remi_manifest_path};
use crate::config::manifest::{Assertion, JsonExpectation, TestStep};
use crate::engine::{
    assertions::evaluate_assertion,
    variables::{expand_assertion, expand_variables},
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

const QUERY: &str = "SELECT path, noreplace, status, source FROM config_files WHERE trove_id = (SELECT id FROM troves WHERE name = 'phase4-daily-driver-corpus') ORDER BY path";
const CONFIG_PATHS: &[&str] = &[
    "/etc/phase4-corpus/app-deleted.conf",
    "/etc/phase4-corpus/app-local.conf",
    "/etc/phase4-corpus/app.conf",
];
const LANES: &[(&str, &str)] = &[
    ("fedora44", "rpm"),
    ("ubuntu-26.04", "deb"),
    ("arch", "arch"),
];

#[test]
fn native_corpus_tnpm15_config_rows_require_exact_json_rows() {
    let manifest = load_manifest(&remi_manifest_path(
        "phase4-native-daily-driver-corpus.toml",
    ))
    .expect("load native daily-driver corpus manifest");
    let test = manifest
        .test
        .iter()
        .find(|test| test.id == "TNPM15")
        .expect("TNPM15");
    assert_eq!(test.step.len(), 10, "TNPM15 must retain its ten-step order");
    assert!(
        manifest
            .distro_overrides
            .values()
            .all(|overrides| !overrides.contains_key("native_corpus_config_count"))
    );

    let step = &test.step[5];
    let command = step.run.as_deref().expect("TNPM15 config query");
    let expected_command = format!("sqlite3 -json ${{DB_PATH}} \"{QUERY}\"");
    assert_eq!(command, expected_command);
    let query = sqlite_query(step);
    assert_eq!(query, QUERY);
    assert!(!query.contains(';'));
    let assertion = typed_root_assertion(step);

    for &(distro, source) in LANES {
        let overrides = manifest
            .distro_overrides
            .get(distro)
            .unwrap_or_else(|| panic!("missing {distro} overrides"));
        assert_eq!(
            overrides
                .get("native_corpus_config_source")
                .map(String::as_str),
            Some(source),
            "{distro} config source"
        );
        assert_eq!(expand_variables(command, overrides), expected_command);

        let expanded = expand_assertion(assertion, overrides);
        let expected = expected_rows(source);
        assert!(matches!(
            &expanded.stdout_json.as_ref().unwrap()[0].expected,
            JsonExpectation::Equals(value) if value == &expected
        ));

        let database = fixture_database(source, 0);
        let actual = execute_json_query(&database, query);
        assert_eq!(actual, expected, "{distro} query rows and trove scoping");
        assert!(evaluate_assertion(&expanded, 0, &actual.to_string(), "").is_ok());
        reject_bad_results(&expanded, &expected);

        let overfull_database = fixture_database(source, 10);
        let overfull_rows = execute_json_query(&overfull_database, query);
        assert_eq!(overfull_rows.as_array().unwrap().len(), 13);
        let old_output = legacy_config_stdout(&overfull_rows);
        let old_assertion = old_substring_assertion(source);
        assert!(
            evaluate_assertion(&old_assertion, 0, &old_output, "").is_ok(),
            "the former `3 config rows` substring check accepts thirteen rows"
        );
        assert!(
            evaluate_assertion(&expanded, 0, &overfull_rows.to_string(), "").is_err(),
            "typed row equality must reject thirteen rows"
        );
    }
}

fn sqlite_query(step: &TestStep) -> &str {
    step.run
        .as_deref()
        .and_then(|command| command.strip_prefix("sqlite3 -json ${DB_PATH} \""))
        .and_then(|query| query.strip_suffix('"'))
        .expect("TNPM15 config query must be one quoted sqlite3 JSON command")
}

fn typed_root_assertion(step: &TestStep) -> &Assertion {
    let assertion = step.assert.as_ref().expect("TNPM15 config assertion");
    assert_eq!(assertion.exit_code, Some(0));
    assert!(assertion.exit_code_not.is_none());
    assert!(assertion.stdout_contains.is_none() && assertion.stdout_not_contains.is_none());
    assert!(assertion.stdout_contains_all.is_none() && assertion.stdout_contains_any.is_none());
    assert!(assertion.stdout_contains_if_success.is_none());
    assert!(assertion.stdout_contains_any_if_success.is_none());
    assert!(assertion.stderr_contains.is_none() && assertion.stderr_not_contains.is_none());
    let checks = assertion.stdout_json.as_ref().expect("exact JSON rows");
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].pointer, "");
    assertion
}

fn expected_rows(source: &str) -> Value {
    Value::Array(
        CONFIG_PATHS
            .iter()
            .map(|path| {
                json!({"path": path, "noreplace": 1, "status": "pristine", "source": source})
            })
            .collect(),
    )
}

fn fixture_database(source: &str, extra_matching_rows: usize) -> Connection {
    let database = Connection::open_in_memory().expect("open in-memory SQLite database");
    database
        .execute_batch(
            "CREATE TABLE troves (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
             CREATE TABLE config_files (
                 trove_id INTEGER NOT NULL,
                 path TEXT NOT NULL,
                 noreplace INTEGER NOT NULL,
                 status TEXT NOT NULL,
                 source TEXT NOT NULL,
                 UNIQUE(path)
             );
             INSERT INTO troves VALUES
               (1, 'phase4-daily-driver-corpus'),
               (2, 'unrelated-package');",
        )
        .expect("create config fixture schema");

    for path in CONFIG_PATHS {
        database
            .execute(
                "INSERT INTO config_files VALUES (1, ?1, 1, 'pristine', ?2)",
                params![path, source],
            )
            .expect("insert expected config row");
    }
    for index in 0..extra_matching_rows {
        let path = format!("/etc/phase4-corpus/extra-{index:02}.conf");
        database
            .execute(
                "INSERT INTO config_files VALUES (1, ?1, 1, 'pristine', ?2)",
                params![path, source],
            )
            .expect("insert extra matching config row");
    }
    database
        .execute(
            "INSERT INTO config_files VALUES (2, ?1, 0, 'modified', 'unrelated-source')",
            ["/etc/unrelated-package/app.conf"],
        )
        .expect("insert unrelated trove row with a distinct path");
    database
}

fn execute_json_query(database: &Connection, query: &str) -> Value {
    let mut statement = database
        .prepare(query)
        .expect("prepare extracted manifest SQL");
    let rows = statement
        .query_map([], |row| {
            Ok(json!({
                "path": row.get::<_, String>(0)?,
                "noreplace": row.get::<_, i64>(1)?,
                "status": row.get::<_, String>(2)?,
                "source": row.get::<_, String>(3)?,
            }))
        })
        .expect("execute extracted manifest SQL")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("read extracted manifest SQL rows");
    Value::Array(rows)
}

fn reject_bad_results(assertion: &Assertion, expected: &Value) {
    reject_rows(assertion, &json!([]), "missing rows");
    let mut extra = expected.clone();
    extra.as_array_mut().unwrap().push(expected[0].clone());
    reject_rows(assertion, &extra, "extra row");

    let mut wrong_field = expected.clone();
    let first = wrong_field[0].as_object_mut().unwrap();
    let noreplace = first.remove("noreplace").unwrap();
    first.insert("preserve".into(), noreplace);
    reject_rows(assertion, &wrong_field, "wrong field name");

    let mut wrong_value = expected.clone();
    wrong_value[0]["status"] = json!("modified");
    reject_rows(assertion, &wrong_value, "wrong field value");

    for (field, value) in [
        ("path", json!("/etc/phase4-corpus/wrong.conf")),
        ("source", json!("wrong-source")),
        ("noreplace", json!(0)),
    ] {
        let mut changed = expected.clone();
        changed[0][field] = value;
        reject_rows(assertion, &changed, &format!("wrong {field}"));
    }

    for (value, kind) in [
        (json!(true), "boolean"),
        (json!("1"), "string"),
        (json!(1.0), "decimal"),
    ] {
        let mut wrong_type = expected.clone();
        wrong_type[0]["noreplace"] = value;
        reject_rows(assertion, &wrong_type, &format!("{kind} noreplace"));
    }

    let mut missing_key = expected.clone();
    missing_key[0].as_object_mut().unwrap().remove("source");
    reject_rows(assertion, &missing_key, "missing key");

    let mut extra_key = expected.clone();
    extra_key[0]["unexpected"] = json!(true);
    reject_rows(assertion, &extra_key, "extra key");

    let valid = expected.to_string();
    reject_stdout(assertion, "not JSON", "malformed JSON");
    reject_stdout(assertion, &format!("{valid}[]"), "trailing JSON document");
    assert!(
        evaluate_assertion(assertion, 1, &valid, "").is_err(),
        "a failed sqlite3 command must reject valid-looking JSON"
    );
}

fn reject_rows(assertion: &Assertion, rows: &Value, defect: &str) {
    reject_stdout(assertion, &rows.to_string(), defect);
}

fn reject_stdout(assertion: &Assertion, stdout: &str, defect: &str) {
    assert!(
        evaluate_assertion(assertion, 0, stdout, "").is_err(),
        "must reject {defect}: {stdout}"
    );
}

fn old_substring_assertion(source: &str) -> Assertion {
    Assertion {
        exit_code: Some(0),
        stdout_contains_all: Some(
            std::iter::once("3 config rows".to_owned())
                .chain(
                    CONFIG_PATHS
                        .iter()
                        .map(|path| format!("{path}|1|pristine|{source}")),
                )
                .collect(),
        ),
        ..Assertion::default()
    }
}

fn legacy_config_stdout(rows: &Value) -> String {
    let rows = rows.as_array().expect("JSON query returns rows");
    let projections = rows
        .iter()
        .map(|row| {
            format!(
                "{}|{}|{}|{}",
                row["path"].as_str().unwrap(),
                row["noreplace"],
                row["status"].as_str().unwrap(),
                row["source"].as_str().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    format!("{} config rows\n{}", rows.len(), projections.join("\n"))
}
