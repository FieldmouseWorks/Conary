// apps/conary-test/src/config/tests/native_corpus/typed_capability.rs
#![cfg(test)]

use super::super::{load_manifest, remi_manifest_path};
use crate::config::manifest::{Assertion, JsonExpectation, TestStep};
use crate::engine::assertions::evaluate_assertion;
use rusqlite::Connection;
use serde_json::{Value, json};

const QUERY: &str = "SELECT installed_file_capabilities.path AS path, json_type(installed_file_capabilities.capabilities_json) AS capabilities_type, json_array_length(installed_file_capabilities.capabilities_json) AS capabilities_count, capability.value AS capability, installed_file_capabilities.permitted AS permitted, installed_file_capabilities.effective AS effective, installed_file_capabilities.inheritable AS inheritable FROM installed_file_capabilities LEFT JOIN json_each(installed_file_capabilities.capabilities_json) AS capability WHERE installed_file_capabilities.trove_id IN (SELECT id FROM troves WHERE name = 'phase4-w7-capability-corpus') ORDER BY installed_file_capabilities.path, capability.key";
const NAME_SELECTOR: &str = "SELECT id FROM troves WHERE name = 'phase4-w7-capability-corpus'";
const PATH: &str = "/usr/bin/phase4-w7-capability";
const CAPABILITY: &str = "cap_net_bind_service";

#[test]
fn native_corpus_file_capability_requires_exact_persisted_rows() {
    let path = remi_manifest_path("phase4-native-daily-driver-corpus.toml");
    let manifest = load_manifest(&path).unwrap();
    let test = manifest
        .test
        .iter()
        .find(|test| test.id == "TNPM21")
        .unwrap();
    assert_eq!(test.step.len(), 5);
    let metadata = &test.step[2];
    let command = metadata
        .run
        .as_deref()
        .expect("persisted capability step must run sqlite3");
    assert_eq!(command, format!("sqlite3 -json ${{DB_PATH}} \"{QUERY}\""));
    assert_eq!(sqlite_query(metadata), QUERY);
    assert_eq!(QUERY.matches("LEFT JOIN json_each(").count(), 1);
    assert!(QUERY.contains("ORDER BY installed_file_capabilities.path, capability.key"));
    assert!(QUERY.contains(&format!("trove_id IN ({NAME_SELECTOR})")));
    assert!(!QUERY.contains(';'));

    let assertion = typed_root_assertion(metadata);
    let valid = expected_rows();
    assert!(evaluate_assertion(assertion, 0, &valid.to_string(), "").is_ok());
    prove_name_selector_covers_every_same_name_trove(sqlite_query(metadata), assertion, &valid);
    demonstrate_substring_false_positive(assertion, &valid);
    reject_bad_results(assertion, &valid);

    assert_eq!(
        test.step[3].run.as_deref(),
        Some(
            "/opt/remi-tests/fixtures/native/assert-selected-generation.py --root /conary --expect-xattr-hex /usr/bin/phase4-w7-capability=security.capability,0100000200040000000000000000000000000000"
        )
    );
    assert_eq!(
        test.step[4].run.as_deref(),
        Some(
            "/opt/remi-tests/fixtures/native/write-corpus-evidence.py /tmp/w7-capability/native-fixture-manifest.json /tmp/conary-corpus-w7-capability.json fedora-44 rpm phase4-w7-capability-corpus 1.0.0-1 x86_64 installation"
        )
    );
}

fn prove_name_selector_covers_every_same_name_trove(
    query: &str,
    assertion: &Assertion,
    expected: &Value,
) {
    let connection = Connection::open_in_memory().expect("open in-memory SQLite database");
    connection
        .execute_batch(
            r#"
            CREATE TABLE troves (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                version TEXT NOT NULL
            );
            CREATE TABLE installed_file_capabilities (
                trove_id INTEGER NOT NULL,
                path TEXT NOT NULL,
                capabilities_json TEXT NOT NULL,
                permitted INTEGER NOT NULL,
                effective INTEGER NOT NULL,
                inheritable INTEGER NOT NULL
            );
            INSERT INTO troves VALUES
                (1, 'phase4-w7-capability-corpus', '1.0.0-1'),
                (2, 'phase4-w7-capability-corpus', '2.0.0-1');
            INSERT INTO installed_file_capabilities VALUES
                (1, '/usr/bin/phase4-w7-capability', '["cap_net_bind_service"]', 1, 1, 0),
                (2, '/usr/bin/phase4-w7-capability', '["cap_sys_admin"]', 1, 1, 0);
            "#,
        )
        .expect("create two-version capability fixture");
    let old_selector = format!("trove_id = ({NAME_SELECTOR})");
    let all_same_name_selector = format!("trove_id IN ({NAME_SELECTOR})");
    let old_query = query.replace(&all_same_name_selector, &old_selector);
    assert_ne!(old_query, query, "test legacy selector");
    let old_rows = execute_json_query(&connection, &old_query);
    assert_eq!(
        &old_rows, expected,
        "the old selector hides the second version"
    );
    assert!(evaluate_assertion(assertion, 0, &old_rows.to_string(), "").is_ok());
    let repaired_rows = execute_json_query(&connection, query);
    assert_eq!(
        repaired_rows.as_array().map(Vec::len),
        Some(2),
        "the repaired selector must return rows for both same-name troves"
    );
    assert!(
        evaluate_assertion(assertion, 0, &repaired_rows.to_string(), "").is_err(),
        "exact JSON equality must reject the extra capability on the second version"
    );
}

fn execute_json_query(connection: &Connection, query: &str) -> Value {
    let mut statement = connection.prepare(query).expect("prepare parsed SQL");
    let rows = statement
        .query_map([], |row| {
            Ok(json!({
                "path": row.get::<_, String>(0)?,
                "capabilities_type": row.get::<_, String>(1)?,
                "capabilities_count": row.get::<_, i64>(2)?,
                "capability": row.get::<_, String>(3)?,
                "permitted": row.get::<_, i64>(4)?,
                "effective": row.get::<_, i64>(5)?,
                "inheritable": row.get::<_, i64>(6)?,
            }))
        })
        .expect("execute parsed manifest SQL")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("read parsed manifest SQL rows");
    Value::Array(rows)
}

fn sqlite_query(step: &TestStep) -> &str {
    step.run
        .as_deref()
        .and_then(|command| command.strip_prefix("sqlite3 -json ${DB_PATH} \""))
        .and_then(|query| query.strip_suffix('\"'))
        .expect("capability query must use sqlite3 -json")
}

fn typed_root_assertion(step: &TestStep) -> &Assertion {
    let assertion = step
        .assert
        .as_ref()
        .expect("persisted capability query needs an assertion");
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
        .expect("persisted capability query must use exact JSON equality");
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].pointer, "");
    assert_eq!(checks[0].expected, JsonExpectation::Equals(expected_rows()));
    assertion
}

fn expected_rows() -> Value {
    json!([{
        "path": PATH,
        "capabilities_type": "array",
        "capabilities_count": 1,
        "capability": CAPABILITY,
        "permitted": 1,
        "effective": 1,
        "inheritable": 0,
    }])
}

fn demonstrate_substring_false_positive(assertion: &Assertion, expected: &Value) {
    let old_row = format!("{PATH}|[\"{CAPABILITY}\"]|1|1|0");
    let old_assertion = Assertion {
        exit_code: Some(0),
        stdout_contains: Some(old_row.clone()),
        ..Assertion::default()
    };
    let stdout = format!("{old_row}\n/usr/bin/phase4-w7-capability-empty|[]|1|1|0");
    assert!(
        evaluate_assertion(&old_assertion, 0, &stdout, "").is_ok(),
        "the old substring accepted the expected row plus an extra empty-array row"
    );

    let extra_empty_array = json!([
        expected[0].clone(),
        {
            "path": "/usr/bin/phase4-w7-capability-empty",
            "capabilities_type": "array",
            "capabilities_count": 0,
            "capability": null,
            "permitted": 1,
            "effective": 1,
            "inheritable": 0,
        },
    ]);
    rejects(
        assertion,
        &extra_empty_array,
        "extra persisted row with an empty capability array",
    );
}

fn reject_bad_results(assertion: &Assertion, expected: &Value) {
    for (field, value) in [
        ("path", json!("/usr/bin/wrong-capability")),
        ("capabilities_type", json!("object")),
        ("capabilities_count", json!(2)),
        ("capability", json!("cap_sys_admin")),
        ("permitted", json!(0)),
        ("effective", json!(0)),
        ("inheritable", json!(1)),
    ] {
        let mut changed = expected.clone();
        changed[0][field] = value;
        rejects(assertion, &changed, field);
    }

    for (field, value) in [
        ("path", Value::Null),
        ("path", json!(7)),
        ("capabilities_type", Value::Null),
        ("capabilities_type", json!(true)),
        ("capabilities_count", Value::Null),
        ("capabilities_count", json!("1")),
        ("capabilities_count", json!(1.0)),
        ("capability", Value::Null),
        ("capability", json!(false)),
        ("permitted", Value::Null),
        ("permitted", json!("1")),
        ("permitted", json!(1.0)),
        ("effective", Value::Null),
        ("effective", json!("1")),
        ("effective", json!(1.0)),
        ("inheritable", Value::Null),
        ("inheritable", json!("0")),
        ("inheritable", json!(0.0)),
    ] {
        let mut changed = expected.clone();
        changed[0][field] = value;
        rejects(assertion, &changed, &format!("invalid {field} type"));
    }

    let extra_capability = json!([
        {
            "path": PATH,
            "capabilities_type": "array",
            "capabilities_count": 2,
            "capability": CAPABILITY,
            "permitted": 1,
            "effective": 1,
            "inheritable": 0,
        },
        {
            "path": PATH,
            "capabilities_type": "array",
            "capabilities_count": 2,
            "capability": "cap_sys_admin",
            "permitted": 1,
            "effective": 1,
            "inheritable": 0,
        },
    ]);
    rejects(assertion, &extra_capability, "extra capability");
    rejects(assertion, &json!([]), "missing row");

    let mut missing_key = expected.clone();
    missing_key[0].as_object_mut().unwrap().remove("capability");
    rejects(assertion, &missing_key, "missing key");
    let mut extra_key = expected.clone();
    extra_key[0]["unexpected"] = json!(true);
    rejects(assertion, &extra_key, "extra key");
    let valid = expected.to_string();
    rejects_stdout(assertion, "not JSON", "malformed JSON");
    rejects_stdout(
        assertion,
        &format!("{valid}{valid}"),
        "concatenated JSON documents",
    );
    assert!(
        evaluate_assertion(assertion, 1, &valid, "").is_err(),
        "must reject a failed sqlite3 command with otherwise valid JSON"
    );
}

fn rejects(assertion: &Assertion, stdout: &Value, defect: &str) {
    rejects_stdout(assertion, &stdout.to_string(), defect);
}

fn rejects_stdout(assertion: &Assertion, stdout: &str, defect: &str) {
    assert!(
        evaluate_assertion(assertion, 0, stdout, "").is_err(),
        "must reject {defect}: {stdout}"
    );
}
