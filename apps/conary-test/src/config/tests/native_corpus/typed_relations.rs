// apps/conary-test/src/config/tests/native_corpus/typed_relations.rs
#![cfg(test)]

use super::super::{load_manifest, remi_manifest_path};
use crate::config::manifest::{Assertion, JsonExpectation, TestStep};
use crate::engine::assertions::evaluate_assertion;
use rusqlite::{Connection, params, types::ValueRef};
use serde_json::{Value, json};

const NAME: &str = "phase4-w7-rpm-semantics";

#[test]
fn native_corpus_rpm_relations_require_exact_persisted_rows() {
    let manifest = load_manifest(&remi_manifest_path(
        "phase4-native-daily-driver-corpus.toml",
    ))
    .expect("load native daily-driver corpus manifest");
    let test = manifest
        .test
        .iter()
        .find(|test| test.id == "TNPM22")
        .expect("TNPM22 must own native RPM relations");
    assert_eq!(test.depends_on.as_ref().unwrap(), &["TNPM21".to_owned()]);
    assert_eq!(test.step.len(), 6);
    let metadata = &test.step[2];
    let query = sqlite_query(metadata);
    assert!(query.contains("FROM troves AS t\nLEFT JOIN package_requirement_groups AS g"));
    assert!(query.contains(
        "ON g.trove_id = t.id AND g.kind IN ('conflict', 'breaks', 'replace', 'obsolete')"
    ));
    assert!(query.contains("WHERE t.name = 'phase4-w7-rpm-semantics'"));
    assert!(query.contains("ORDER BY t.id, g.kind, g.id"));
    assert!(!query.contains(';'));

    let assertion = typed_root_assertion(metadata);
    let expected = expected_rows();
    let database = fixture_database();
    let actual = execute_json_query(&database, query).expect("execute parsed manifest SQL");
    assert_eq!(
        actual, expected,
        "manifest SQL must project the fixture rows"
    );
    assert!(evaluate_assertion(assertion, 0, &actual.to_string(), "").is_ok());

    for (mutation, defect) in [
        (
            "UPDATE package_requirement_groups SET requirement_json = json_set(requirement_json, '$.expression.operands.version_constraint', '< 20', '$.alternatives[0].version_constraint', '< 20', '$.native_text', 'phase4-w7-conflict < 20') WHERE kind = 'conflict'",
            "wrong RPM comparison boundary",
        ),
        (
            "UPDATE package_requirement_groups SET version_scheme = 'debian' WHERE kind = 'conflict'",
            "wrong persisted relation scheme",
        ),
        (
            "UPDATE troves SET version_scheme = 'debian' WHERE id = 1",
            "wrong installed trove scheme",
        ),
        (
            "UPDATE package_requirement_groups SET kind = 'replace' WHERE kind = 'obsolete'",
            "wrong negative relation kind",
        ),
        (
            "UPDATE package_requirement_groups SET requirement_json = json_set(requirement_json, '$.kind', 'Depends') WHERE kind = 'conflict'",
            "wrong typed payload kind",
        ),
        (
            "UPDATE package_requirement_groups SET requirement_json = json_set(requirement_json, '$.behavior', 'Conditional') WHERE kind = 'conflict'",
            "wrong typed behavior",
        ),
        (
            "UPDATE package_requirement_groups SET requirement_json = json_set(requirement_json, '$.expression.operator', 'or') WHERE kind = 'conflict'",
            "wrong expression operator",
        ),
        (
            "UPDATE package_requirement_groups SET requirement_json = json_set(requirement_json, '$.expression.operands.name', 'wrong-name') WHERE kind = 'conflict'",
            "wrong authoritative expression name",
        ),
        (
            "UPDATE package_requirement_groups SET requirement_json = json_set(requirement_json, '$.alternatives[0].name', 'wrong-name') WHERE kind = 'conflict'",
            "disagreeing alternative index",
        ),
        (
            "UPDATE package_requirement_groups SET requirement_json = json_set(requirement_json, '$.alternatives', json_array(1, 2)) WHERE kind = 'conflict'",
            "extra alternative",
        ),
        (
            "UPDATE package_requirement_groups SET requirement_json = json_set(requirement_json, '$.expression.operands.capability_kind', 'PackageName') WHERE kind = 'conflict'",
            "wrong capability kind",
        ),
        (
            "UPDATE package_requirement_groups SET requirement_json = json_set(requirement_json, '$.expression.operands.architecture_qualifier.kind', 'exact') WHERE kind = 'conflict'",
            "wrong architecture qualifier",
        ),
        (
            "UPDATE troves SET version = '1.0.0-2' WHERE id = 1",
            "wrong installed version",
        ),
        (
            "UPDATE troves SET architecture = 'aarch64' WHERE id = 1",
            "wrong installed architecture",
        ),
        (
            "UPDATE troves SET source_profile = 'arch' WHERE id = 1",
            "wrong source profile",
        ),
        (
            "UPDATE troves SET type = 'component' WHERE id = 1",
            "wrong trove type",
        ),
        (
            "UPDATE troves SET install_source = 'repository' WHERE id = 1",
            "wrong install source",
        ),
        (
            "DELETE FROM package_requirement_groups WHERE kind = 'obsolete' AND trove_id = 1",
            "missing relation",
        ),
        ("DELETE FROM troves WHERE id = 1", "missing named trove"),
        (
            "INSERT INTO troves VALUES (3, 'phase4-w7-rpm-semantics', '2.0.0-1', NULL, 'package', 'x86_64', 'rpm', 'fedora-44', 'file', 'explicit')",
            "second same-name version without relations",
        ),
        (
            "INSERT INTO package_requirement_groups (trove_id, kind, version_scheme, requirement_json) SELECT trove_id, 'breaks', version_scheme, json_set(requirement_json, '$.kind', 'Breaks') FROM package_requirement_groups WHERE kind = 'conflict' AND trove_id = 1",
            "extra negative relation",
        ),
        (
            "INSERT INTO package_requirement_groups (trove_id, kind, version_scheme, requirement_json) SELECT trove_id, kind, version_scheme, json_set(requirement_json, '$.expression.operands.name', 'phase4-w7-extra', '$.alternatives[0].name', 'phase4-w7-extra', '$.native_text', 'phase4-w7-extra < 2') FROM package_requirement_groups WHERE kind = 'conflict' AND trove_id = 1",
            "second distinct conflict group",
        ),
    ] {
        let database = fixture_database();
        database.execute_batch(mutation).expect(defect);
        let rows = execute_json_query(&database, query).expect(defect);
        rejects(assertion, &rows, defect);
    }

    let database = fixture_database();
    database
        .execute(
            "UPDATE package_requirement_groups SET requirement_json = '{broken' WHERE kind = 'conflict'",
            [],
        )
        .unwrap();
    assert!(
        execute_json_query(&database, query).is_err(),
        "malformed persisted JSON must fail sqlite3 before assertion"
    );

    demonstrate_substring_false_positive(assertion, &expected);
    reject_bad_stdout(assertion, &expected);
}

fn sqlite_query(step: &TestStep) -> &str {
    step.run
        .as_deref()
        .map(str::trim)
        .and_then(|command| command.strip_prefix("sqlite3 -json ${DB_PATH} \""))
        .and_then(|query| query.strip_suffix('"'))
        .expect("TNPM22 must use one sqlite3 -json command")
}

fn typed_root_assertion(step: &TestStep) -> &Assertion {
    let assertion = step.assert.as_ref().expect("TNPM22 needs an assertion");
    assert_eq!(assertion.exit_code, Some(0));
    assert!(assertion.exit_code_not.is_none());
    assert!(assertion.stdout_contains.is_none());
    assert!(assertion.stdout_contains_all.is_none());
    assert!(assertion.stdout_contains_any.is_none());
    assert!(assertion.stdout_not_contains.is_none());
    assert!(assertion.stdout_contains_if_success.is_none());
    assert!(assertion.stdout_contains_any_if_success.is_none());
    assert!(assertion.stderr_contains.is_none());
    assert!(assertion.stderr_not_contains.is_none());
    let checks = assertion
        .stdout_json
        .as_ref()
        .expect("exact JSON assertion");
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].pointer, "");
    assert_eq!(checks[0].expected, JsonExpectation::Equals(expected_rows()));
    assertion
}

fn expected_rows() -> Value {
    json!([
        expected_row("conflict", "Conflict", "phase4-w7-conflict", "< 2"),
        expected_row("obsolete", "Obsolete", "phase4-w7-replaced", "<= 1"),
    ])
}

fn expected_row(kind: &str, payload_kind: &str, name: &str, constraint: &str) -> Value {
    json!({
        "trove_name": NAME, "trove_version": "1.0.0-1", "trove_type": "package",
        "trove_architecture": "x86_64", "trove_scheme": "rpm", "source_profile": "fedora-44",
        "install_source": "file", "install_reason": "explicit", "group_kind": kind,
        "group_scheme": "rpm", "group_type": "object", "payload_kind": payload_kind,
        "behavior": "Hard", "expression_type": "object", "expression_operator": "atom",
        "operand_type": "object", "expression_name": name, "expression_constraint": constraint,
        "expression_capability_kind_type": "null", "expression_architecture_kind": "unqualified",
        "expression_native_text_type": "null", "alternatives_type": "array", "alternatives_count": 1,
        "alternative_type": "object", "alternative_name": name, "alternative_constraint": constraint,
        "alternative_capability_kind_type": "null", "alternative_architecture_kind": "unqualified",
        "alternative_native_text_type": "null", "native_text": format!("{name} {constraint}"),
    })
}

fn fixture_database() -> Connection {
    let database = Connection::open_in_memory().expect("open disposable SQLite database");
    database
        .execute_batch(
            "CREATE TABLE troves (id INTEGER PRIMARY KEY, name TEXT NOT NULL, version TEXT NOT NULL, package_release TEXT, type TEXT NOT NULL, architecture TEXT, version_scheme TEXT NOT NULL, source_profile TEXT, install_source TEXT NOT NULL, install_reason TEXT NOT NULL);
             CREATE UNIQUE INDEX idx_troves_exact_identity ON troves(name, version, COALESCE(package_release, ''), COALESCE(architecture, ''));
             CREATE TABLE package_requirement_groups (id INTEGER PRIMARY KEY, trove_id INTEGER NOT NULL, kind TEXT NOT NULL, version_scheme TEXT NOT NULL, requirement_json TEXT NOT NULL, UNIQUE(trove_id, kind, requirement_json));
             INSERT INTO troves VALUES
               (1, 'phase4-w7-rpm-semantics', '1.0.0-1', NULL, 'package', 'x86_64', 'rpm', 'fedora-44', 'file', 'explicit'),
               (2, 'unrelated-package', '1.0.0-1', NULL, 'package', 'x86_64', 'rpm', 'fedora-44', 'file', 'explicit');",
        )
        .expect("create typed relation fixture schema");
    insert_group(
        &database,
        1,
        "conflict",
        "Conflict",
        "phase4-w7-conflict",
        "< 2",
    );
    insert_group(
        &database,
        1,
        "obsolete",
        "Obsolete",
        "phase4-w7-replaced",
        "<= 1",
    );
    insert_group(
        &database,
        2,
        "conflict",
        "Conflict",
        "unrelated-package",
        "< 99",
    );
    database
}

fn insert_group(
    database: &Connection,
    trove_id: i64,
    kind: &str,
    payload_kind: &str,
    name: &str,
    constraint: &str,
) {
    let clause = json!({
        "name": name,
        "capability_kind": null,
        "version_constraint": constraint,
        "architecture_qualifier": {"kind": "unqualified"},
        "native_text": null,
    });
    let group = json!({
        "kind": payload_kind, "behavior": "Hard",
        "expression": {"operator": "atom", "operands": clause},
        "alternatives": [clause], "description": null,
        "native_text": format!("{name} {constraint}"),
    });
    database
        .execute(
            "INSERT INTO package_requirement_groups (trove_id, kind, version_scheme, requirement_json) VALUES (?1, ?2, 'rpm', ?3)",
            params![trove_id, kind, group.to_string()],
        )
        .expect("insert typed relation group");
}

fn execute_json_query(database: &Connection, query: &str) -> rusqlite::Result<Value> {
    let mut statement = database.prepare(query)?;
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
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Value::Array(rows))
}

fn demonstrate_substring_false_positive(assertion: &Assertion, expected: &Value) {
    let old = Assertion {
        exit_code: Some(0),
        stdout_contains_all: Some(vec![
            "conflict|phase4-w7-conflict|< 2".into(),
            "obsolete|phase4-w7-replaced|<= 1".into(),
        ]),
        ..Assertion::default()
    };
    let wrong = "conflict|phase4-w7-conflict|< 20\nobsolete|phase4-w7-replaced|<= 1";
    assert!(
        evaluate_assertion(&old, 0, wrong, "").is_ok(),
        "the old substring accepted the wrong RPM comparison boundary"
    );
    let mut wrong = expected.clone();
    wrong[0]["expression_constraint"] = json!("< 20");
    rejects(assertion, &wrong, "wrong comparison boundary");
}

fn reject_bad_stdout(assertion: &Assertion, expected: &Value) {
    rejects(assertion, &json!([]), "missing rows");
    let mut one_row = expected.clone();
    one_row.as_array_mut().unwrap().pop();
    rejects(assertion, &one_row, "missing obsolete row");
    let mut duplicate = expected.clone();
    duplicate.as_array_mut().unwrap().push(expected[0].clone());
    rejects(assertion, &duplicate, "duplicate row");
    let mut wrong_type = expected.clone();
    wrong_type[0]["alternatives_count"] = json!("1");
    rejects(assertion, &wrong_type, "wrong JSON type");
    let mut missing_key = expected.clone();
    missing_key[0]
        .as_object_mut()
        .unwrap()
        .remove("expression_name");
    rejects(assertion, &missing_key, "missing projected field");
    let mut extra_key = expected.clone();
    extra_key[0]["unexpected"] = json!(true);
    rejects(assertion, &extra_key, "extra projected field");
    let valid = expected.to_string();
    rejects_stdout(assertion, "not JSON", "malformed output");
    rejects_stdout(
        assertion,
        &format!("{valid}{valid}"),
        "concatenated documents",
    );
    assert!(
        evaluate_assertion(assertion, 1, &valid, "").is_err(),
        "a failed sqlite3 command must reject valid-looking output"
    );
}

fn rejects(assertion: &Assertion, rows: &Value, defect: &str) {
    rejects_stdout(assertion, &rows.to_string(), defect);
}

fn rejects_stdout(assertion: &Assertion, stdout: &str, defect: &str) {
    assert!(
        evaluate_assertion(assertion, 0, stdout, "").is_err(),
        "must reject {defect}: {stdout}"
    );
}
