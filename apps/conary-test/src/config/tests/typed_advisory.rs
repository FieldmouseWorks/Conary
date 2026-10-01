// apps/conary-test/src/config/tests/typed_advisory.rs
#![cfg(test)]

use super::{load_manifest, remi_manifest_path};
use crate::config::manifest::{Assertion, JsonExpectation, TestStep};
use crate::engine::{
    assertions::evaluate_assertion,
    variables::{expand_assertion, expand_variables},
};
use rusqlite::{Connection, types::ValueRef};
use serde_json::{Value, json};

const LANES: &[(&str, &str, &str)] = &[
    ("fedora44", "x86_64", "1.0.1-1"),
    ("ubuntu-26.04", "amd64", "1.0.1"),
    ("arch", "x86_64", "1.0.1-1"),
];

#[test]
fn security_advisory_pipeline_requires_exact_persisted_rows_for_all_lanes() {
    let manifest = load_manifest(&remi_manifest_path(
        "phase4-security-advisory-pipeline.toml",
    ))
    .expect("load security advisory pipeline manifest");
    let test = manifest
        .test
        .iter()
        .find(|test| test.id == "TSEC05")
        .expect("TSEC05 must own persisted advisory metadata");
    assert_eq!(test.step.len(), 2);

    let setup = &test.step[0];
    let setup_run = setup.run.as_deref().expect("repository setup command");
    let positions =
        ["repo remove", "repo add", "repo sync"].map(|command| setup_run.find(command).unwrap());
    assert!(positions[0] < positions[1] && positions[1] < positions[2]);
    assert!(setup_run.contains("|| true\nset -e\n"));
    assert_eq!(setup.assert.as_ref().unwrap().exit_code, Some(0));

    let query_step = &test.step[1];
    let query = sqlite_query(query_step);
    assert!(query.contains("FROM repositories r JOIN repository_packages rp"));
    assert!(query.contains("ORDER BY rp.architecture, rp.id"));
    assert!(!query.contains("rp.architecture ="));

    let assertion = typed_root_assertion(query_step);
    for &(distro, architecture, version) in LANES {
        let vars = manifest
            .distro_overrides
            .get(distro)
            .unwrap_or_else(|| panic!("missing {distro} overrides"));
        assert_eq!(vars["native_arch"], architecture);
        assert_eq!(vars["native_update_version"], version);
        let expanded_query = expand_variables(query, vars);
        assert!(!expanded_query.contains("${native_"));

        let expanded_assertion = expand_assertion(assertion, vars);
        let expected = match &expanded_assertion.stdout_json.as_ref().unwrap()[0].expected {
            JsonExpectation::Equals(value) => value,
            JsonExpectation::Null => panic!("TSEC05 must expect one exact row"),
        };
        assert_eq!(
            expected[0]["architecture"], architecture,
            "{distro} architecture expansion"
        );
        assert_eq!(
            expected[0]["fixed_version"], version,
            "{distro} version expansion"
        );

        let connection = advisory_database();
        for (id, repository_id, package, row_version) in [
            (1, 1, "phase4-runtime-fixture", version),
            (2, 2, "phase4-runtime-fixture", version),
            (3, 1, "other-package", version),
            (4, 1, "phase4-runtime-fixture", "9.9.9"),
        ] {
            insert_advisory_row(
                &connection,
                id,
                repository_id,
                package,
                row_version,
                architecture,
            );
        }

        let valid = execute_json_query(&connection, &expanded_query);
        assert_eq!(&valid, expected, "{distro} parsed SQL result");
        assert!(evaluate_assertion(&expanded_assertion, 0, &valid.to_string(), "").is_ok());

        connection
            .execute(
                "UPDATE repositories SET security_advisory_support = 'unknown' WHERE id = 1",
                [],
            )
            .unwrap();
        let unauthorized = execute_json_query(&connection, &expanded_query);
        rejects(&expanded_assertion, &unauthorized, "changed local support");
        connection
            .execute(
                "UPDATE repositories SET security_advisory_support = 'supported' WHERE id = 1",
                [],
            )
            .unwrap();

        insert_advisory_row(
            &connection,
            5,
            1,
            "phase4-runtime-fixture",
            version,
            "arm64",
        );
        let extra_architecture = execute_json_query(&connection, &expanded_query);
        assert_eq!(extra_architecture.as_array().unwrap().len(), 2);
        rejects(
            &expanded_assertion,
            &extra_architecture,
            "extra architecture row",
        );

        demonstrate_substring_decoy(&expanded_assertion, expected);
        reject_bad_results(&expanded_assertion, expected);
    }
}

fn sqlite_query(step: &TestStep) -> &str {
    step.run
        .as_deref()
        .and_then(|command| command.strip_prefix("sqlite3 -json ${DB_PATH} \""))
        .and_then(|query| query.trim_end().strip_suffix('"'))
        .expect("TSEC05 persisted query must use sqlite3 -json")
}

fn typed_root_assertion(step: &TestStep) -> &Assertion {
    let assertion = step.assert.as_ref().expect("query must assert its result");
    assert_eq!(assertion.exit_code, Some(0));
    assert!(assertion.exit_code_not.is_none());
    assert!(
        assertion.stdout_contains.is_none()
            && assertion.stdout_not_contains.is_none()
            && assertion.stdout_contains_all.is_none()
            && assertion.stdout_contains_any.is_none()
            && assertion.stdout_contains_if_success.is_none()
            && assertion.stdout_contains_any_if_success.is_none()
            && assertion.stderr_contains.is_none()
            && assertion.stderr_not_contains.is_none()
    );
    let checks = assertion
        .stdout_json
        .as_ref()
        .expect("exact JSON assertion");
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].pointer, "");
    assert!(matches!(checks[0].expected, JsonExpectation::Equals(_)));
    assertion
}

fn advisory_database() -> Connection {
    let connection = Connection::open_in_memory().expect("open disposable SQLite database");
    connection
        .execute_batch(
            "CREATE TABLE repositories (id INTEGER PRIMARY KEY, name TEXT NOT NULL, security_advisory_support TEXT NOT NULL);
             CREATE TABLE repository_packages (id INTEGER PRIMARY KEY, repository_id INTEGER NOT NULL, name TEXT NOT NULL, version TEXT NOT NULL, architecture TEXT, is_security_update INTEGER NOT NULL, severity TEXT, cve_ids TEXT, advisory_id TEXT, advisory_url TEXT, metadata TEXT);
             INSERT INTO repositories VALUES (1, 'security-advisory-json', 'supported'), (2, 'other-repository', 'supported');",
        )
        .expect("create advisory query fixture schema");
    connection
}

fn insert_advisory_row(
    connection: &Connection,
    id: i64,
    repository_id: i64,
    package: &str,
    version: &str,
    architecture: &str,
) {
    let metadata = format!(
        r#"{{"security_advisory":{{"id":"TEST-2026-0001","source":"conary-json","source_trust":"trusted","severity":"critical","cves":["CVE-2026-0001"],"fixed_version":"{version}","url":"https://security.example.test/TEST-2026-0001"}}}}"#
    );
    connection
        .execute(
            "INSERT INTO repository_packages (id, repository_id, name, version, architecture, is_security_update, severity, cve_ids, advisory_id, advisory_url, metadata) VALUES (?1, ?2, ?3, ?4, ?5, 1, 'critical', 'CVE-2026-0001', 'TEST-2026-0001', 'https://security.example.test/TEST-2026-0001', ?6)",
            rusqlite::params![id, repository_id, package, version, architecture, metadata],
        )
        .expect("insert advisory query fixture row");
}

fn execute_json_query(connection: &Connection, query: &str) -> Value {
    let mut statement = connection.prepare(query).expect("prepare manifest SQL");
    let columns = statement
        .column_names()
        .iter()
        .map(|name| name.to_string())
        .collect::<Vec<_>>();
    let rows = statement
        .query_map([], |row| {
            let mut object = serde_json::Map::new();
            for (index, name) in columns.iter().enumerate() {
                let value = match row.get_ref(index)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(number) => json!(number),
                    ValueRef::Real(number) => json!(number),
                    ValueRef::Text(text) => json!(std::str::from_utf8(text).unwrap()),
                    ValueRef::Blob(_) => Value::Null,
                };
                object.insert(name.clone(), value);
            }
            Ok(Value::Object(object))
        })
        .expect("execute manifest SQL")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("read manifest SQL results");
    Value::Array(rows)
}

fn demonstrate_substring_decoy(assertion: &Assertion, expected: &Value) {
    let old_assertion = Assertion {
        exit_code: Some(0),
        stdout_contains_all: Some(vec![
            "1|critical|CVE-2026-0001|TEST-2026-0001".into(),
            "fixed_version".into(),
            "source_trust".into(),
            "conary-json".into(),
        ]),
        ..Assertion::default()
    };
    let mut wrong = expected.clone();
    wrong[0]["advisory_id"] = json!("WRONG-ADVISORY");
    let stdout = format!(
        "1|critical|CVE-2026-0001|WRONG-ADVISORY\ndiagnostic decoy: 1|critical|CVE-2026-0001|TEST-2026-0001 fixed_version source_trust conary-json"
    );
    assert!(evaluate_assertion(&old_assertion, 0, &stdout, "").is_ok());
    rejects(assertion, &wrong, "wrong persisted advisory id");
}

fn reject_bad_results(assertion: &Assertion, expected: &Value) {
    for (field, value) in expected[0].as_object().unwrap() {
        let mut changed = expected.clone();
        changed[0][field] = if value.is_string() {
            json!("wrong")
        } else {
            json!(999)
        };
        rejects(assertion, &changed, field);
    }
    for (field, value) in [
        ("is_security_update", json!("1")),
        ("cves_count", json!("1")),
        ("metadata_type", json!(1)),
        ("advisory_type", Value::Null),
        ("cves_type", json!(true)),
        ("source_trust", Value::Null),
    ] {
        let mut changed = expected.clone();
        changed[0][field] = value;
        rejects(assertion, &changed, &format!("wrong type for {field}"));
    }
    for field in expected[0].as_object().unwrap().keys() {
        let mut missing_key = expected.clone();
        missing_key[0].as_object_mut().unwrap().remove(field);
        rejects(assertion, &missing_key, &format!("missing {field}"));
    }

    let mut missing_row = expected.clone();
    missing_row.as_array_mut().unwrap().clear();
    rejects(assertion, &missing_row, "missing row");
    let mut extra_row = expected.clone();
    extra_row.as_array_mut().unwrap().push(expected[0].clone());
    rejects(assertion, &extra_row, "extra row");
    let mut extra_key = expected.clone();
    extra_key[0]["unexpected"] = json!(true);
    rejects(assertion, &extra_key, "extra key");
    rejects_stdout(assertion, "not JSON", "malformed JSON");
    rejects_stdout(
        assertion,
        &format!("{}\n{}", expected, expected),
        "concatenated JSON documents",
    );
    assert!(evaluate_assertion(assertion, 1, &expected.to_string(), "").is_err());
}

fn rejects(assertion: &Assertion, actual: &Value, defect: &str) {
    rejects_stdout(assertion, &actual.to_string(), defect);
}

fn rejects_stdout(assertion: &Assertion, stdout: &str, defect: &str) {
    assert!(
        evaluate_assertion(assertion, 0, stdout, "").is_err(),
        "must reject {defect}"
    );
}
