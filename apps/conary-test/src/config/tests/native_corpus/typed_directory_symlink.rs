// apps/conary-test/src/config/tests/native_corpus/typed_directory_symlink.rs
#![cfg(test)]

use super::super::{load_manifest, remi_manifest_path};
use crate::config::manifest::{Assertion, JsonExpectation, TestStep};
use crate::engine::{
    assertions::evaluate_assertion,
    variables::{expand_assertion, expand_variables},
};
use conary_core::payload::{
    PayloadContentAuthority, PayloadNode, PayloadNodeKind, ResolvedPayloadNode,
};
use rusqlite::types::{Type, ValueRef};
use rusqlite::{Connection, Error as SqliteError, params};
use serde_json::Map;
use serde_json::{Value, json};
use std::io;

const QUERY: &str = r#"SELECT path, json_extract(payload_node_json, '$.source.kind.type') AS kind, CASE WHEN json_extract(payload_node_json, '$.source.kind.type') = 'directory' THEN json_extract(payload_node_json, '$.source.mode') END AS directory_mode, CASE WHEN json_extract(payload_node_json, '$.source.kind.type') = 'symlink' THEN json_extract(payload_node_json, '$.source.kind.target') END AS symlink_target FROM files WHERE trove_id = (SELECT id FROM troves WHERE name = 'phase4-daily-driver-corpus') AND path IN ('/opt', '/usr/bin/phase4-corpus-link', '/usr/lib/phase4-corpus/state') ORDER BY path"#;
const LEGACY_QUERY: &str = r#"SELECT path || '|' || json_extract(payload_node_json, '$.source.kind.type') || '|' || printf('%04o', json_extract(payload_node_json, '$.source.mode') & 4095) FROM files WHERE trove_id = (SELECT id FROM troves WHERE name = 'phase4-daily-driver-corpus') AND path IN ('/opt', '/usr/lib/phase4-corpus/state') ORDER BY path; SELECT path || '|' || json_extract(payload_node_json, '$.source.kind.type') || '|' || json_extract(payload_node_json, '$.source.kind.target') FROM files WHERE trove_id = (SELECT id FROM troves WHERE name = 'phase4-daily-driver-corpus') AND path = '/usr/bin/phase4-corpus-link'"#;
const LANES: &[&str] = &["fedora44", "ubuntu-26.04", "arch"];
const OPT: &str = "/opt";
const LINK: &str = "/usr/bin/phase4-corpus-link";
const STATE: &str = "/usr/lib/phase4-corpus/state";

#[test]
fn native_corpus_tnpm15_directories_and_symlink_require_exact_scoped_json_rows() {
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

    let step = &test.step[2];
    let command = step.run.as_deref().expect("TNPM15 directory/symlink query");
    let expected_command = format!(r#"sqlite3 -json ${{DB_PATH}} "{QUERY}""#);
    assert_eq!(command, expected_command);
    assert_eq!(command.matches("sqlite3").count(), 1);
    let query = sqlite_query(step);
    assert_eq!(query, QUERY);
    assert!(
        !query.contains(';'),
        "the manifest must run one SQL statement"
    );

    let assertion = typed_root_assertion(step);
    let expected = expected_rows();
    for distro in LANES {
        let overrides = manifest
            .distro_overrides
            .get(*distro)
            .unwrap_or_else(|| panic!("missing {distro} overrides"));
        assert_eq!(expand_variables(command, overrides), expected_command);
        let expanded = expand_assertion(assertion, overrides);
        assert_eq!(
            expanded.stdout_json.as_ref().unwrap()[0].expected,
            JsonExpectation::Equals(expected.clone()),
            "{distro} must retain the same exact payload proof"
        );

        let database = positive_database("phase4-corpus");
        let unrelated_rows: i64 = database
            .query_row("SELECT COUNT(*) FROM files WHERE trove_id = 2", [], |row| {
                row.get(0)
            })
            .expect("count unrelated-trove fixture rows");
        assert_eq!(unrelated_rows, 3);
        let actual = execute_json_query(&database, query);
        assert_eq!(actual, expected, "{distro} exact target-trove payload rows");
        assert!(evaluate_assertion(&expanded, 0, &actual.to_string(), "").is_ok());
    }

    prove_trove_scope(query, assertion, &expected);
    reject_wrong_sql_alias(query, assertion);
    reject_invalid_payloads(query, assertion);
    reject_invalid_results(assertion, &expected);
    prove_legacy_false_positive(query, assertion);
    prove_corrupt_mode_type_false_positive(query, assertion);
}

fn sqlite_query(step: &TestStep) -> &str {
    step.run
        .as_deref()
        .and_then(|command| command.strip_prefix("sqlite3 -json ${DB_PATH} \""))
        .and_then(|query| query.strip_suffix('"'))
        .expect("TNPM15 directory/symlink query must be one quoted sqlite3 JSON command")
}

fn typed_root_assertion(step: &TestStep) -> &Assertion {
    let assertion = step.assert.as_ref().expect("directory/symlink assertion");
    assert_eq!(assertion.exit_code, Some(0));
    assert!(assertion.exit_code_not.is_none());
    assert!(assertion.stdout_contains.is_none() && assertion.stdout_not_contains.is_none());
    assert!(assertion.stdout_contains_all.is_none() && assertion.stdout_contains_any.is_none());
    assert!(assertion.stdout_contains_if_success.is_none());
    assert!(assertion.stdout_contains_any_if_success.is_none());
    assert!(assertion.stderr_contains.is_none() && assertion.stderr_not_contains.is_none());
    let checks = assertion.stdout_json.as_ref().expect("exact JSON result");
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].pointer, "", "compare the complete result array");
    assert_eq!(checks[0].expected, JsonExpectation::Equals(expected_rows()));
    assertion
}

fn expected_rows() -> Value {
    json!([
        {
            "path": OPT,
            "kind": "directory",
            "directory_mode": 16872,
            "symlink_target": null
        },
        {
            "path": LINK,
            "kind": "symlink",
            "directory_mode": null,
            "symlink_target": "phase4-corpus"
        },
        {
            "path": STATE,
            "kind": "directory",
            "directory_mode": 16872,
            "symlink_target": null
        }
    ])
}

fn positive_database(symlink_target: &str) -> Connection {
    let database = database();
    insert_payload_node(&database, 1, OPT, directory_node(0o750), None);
    insert_payload_node(&database, 1, LINK, symlink_node(symlink_target), None);
    insert_payload_node(&database, 1, STATE, directory_node(0o750), None);

    insert_payload_node(&database, 2, "/opt/unrelated", directory_node(0o700), None);
    insert_payload_node(
        &database,
        2,
        "/usr/bin/unrelated-link",
        symlink_node("unrelated-target"),
        None,
    );
    insert_payload_node(
        &database,
        2,
        "/usr/share/unrelated/regular",
        PayloadNode::regular(0o644),
        Some(b"unrelated regular payload"),
    );
    database
}

fn database() -> Connection {
    let database = Connection::open_in_memory().expect("open in-memory payload database");
    database
        .execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE troves (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
             CREATE TABLE files (
                 id INTEGER PRIMARY KEY,
                 path TEXT NOT NULL UNIQUE,
                 payload_node_json TEXT NOT NULL CHECK(json_valid(payload_node_json)),
                 content_sha256 TEXT,
                 content_size INTEGER,
                 trove_id INTEGER NOT NULL REFERENCES troves(id),
                 CHECK ((content_sha256 IS NULL AND content_size IS NULL) OR
                        (content_sha256 IS NOT NULL AND content_size IS NOT NULL AND content_size >= 0))
             );
             INSERT INTO troves VALUES
                 (1, 'phase4-daily-driver-corpus'),
                 (2, 'unrelated-package');",
        )
        .expect("create payload fixture schema");
    database
}

fn directory_node(permissions: u32) -> PayloadNode {
    let mut node = PayloadNode::regular(permissions);
    node.kind = PayloadNodeKind::Directory;
    node.mode = 0o040000 | permissions;
    node
}

fn symlink_node(target: &str) -> PayloadNode {
    let mut node = PayloadNode::regular(0o777);
    node.kind = PayloadNodeKind::Symlink {
        target: target.to_owned(),
    };
    node.mode = 0o120000 | 0o777;
    node
}

fn insert_payload_node(
    database: &Connection,
    trove_id: i64,
    path: &str,
    source: PayloadNode,
    content_bytes: Option<&[u8]>,
) {
    let node = ResolvedPayloadNode::from_numeric_source(source)
        .expect("fixture source node resolves numeric ownership");
    let content = content_bytes.map(|bytes| PayloadContentAuthority {
        sha256: conary_core::hash::sha256(bytes),
        size: bytes.len() as u64,
    });
    node.source
        .validate_content(content.as_ref())
        .expect("fixture node has valid content authority");
    let payload = serde_json::to_string(&node).expect("serialize complete resolved payload node");
    let (content_sha256, content_size) = content
        .map(|content| {
            (
                Some(content.sha256),
                Some(i64::try_from(content.size).expect("fixture content size fits SQLite")),
            )
        })
        .unwrap_or((None, None));
    database
        .execute(
            "INSERT INTO files (path, payload_node_json, content_sha256, content_size, trove_id) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![path, payload, content_sha256, content_size, trove_id],
        )
        .expect("insert validated resolved payload node");
}

fn replace_payload_node(database: &Connection, trove_id: i64, path: &str, source: PayloadNode) {
    database
        .execute("DELETE FROM files WHERE path = ?1", [path])
        .expect("remove replaced payload row");
    insert_payload_node(database, trove_id, path, source, None);
}

fn execute_json_query(database: &Connection, query: &str) -> Value {
    let mut statement = database
        .prepare(query)
        .expect("prepare SQL extracted from the manifest");
    let column_names = statement
        .column_names()
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let rows = statement
        .query_map([], |row| {
            let mut object = Map::new();
            for (index, name) in column_names.iter().enumerate() {
                object.insert(name.clone(), sqlite_value_json(row.get_ref(index)?, index)?);
            }
            Ok(Value::Object(object))
        })
        .expect("execute SQL extracted from the manifest")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("read manifest SQL rows");
    Value::Array(rows)
}

fn sqlite_value_json(value: ValueRef<'_>, column_index: usize) -> rusqlite::Result<Value> {
    match value {
        ValueRef::Null => Ok(Value::Null),
        ValueRef::Integer(value) => Ok(json!(value)),
        ValueRef::Real(value) => serde_json::Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| {
                SqliteError::FromSqlConversionFailure(
                    column_index,
                    Type::Real,
                    Box::new(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "non-finite SQLite real cannot be represented in JSON",
                    )),
                )
            }),
        ValueRef::Text(value) => std::str::from_utf8(value)
            .map(|value| Value::String(value.to_owned()))
            .map_err(|error| {
                SqliteError::FromSqlConversionFailure(column_index, Type::Text, Box::new(error))
            }),
        ValueRef::Blob(_) => Err(SqliteError::FromSqlConversionFailure(
            column_index,
            Type::Blob,
            Box::new(io::Error::new(
                io::ErrorKind::InvalidData,
                "this sqlite3 -json payload proof does not emit BLOB values",
            )),
        )),
    }
}

fn prove_trove_scope(query: &str, assertion: &Assertion, expected: &Value) {
    let database = positive_database("phase4-corpus");
    database
        .execute("DELETE FROM files WHERE path = ?1", [OPT])
        .expect("remove target-trove directory for the scope sentinel");
    insert_payload_node(&database, 2, OPT, directory_node(0o750), None);

    let unscoped_query = query.replace(
        "WHERE trove_id = (SELECT id FROM troves WHERE name = 'phase4-daily-driver-corpus') AND ",
        "WHERE ",
    );
    assert_ne!(
        unscoped_query, query,
        "query must carry the exact trove scope"
    );
    assert_eq!(execute_json_query(&database, &unscoped_query), *expected);

    let scoped_rows = execute_json_query(&database, query);
    assert_eq!(scoped_rows.as_array().unwrap().len(), 2);
    assert!(evaluate_assertion(assertion, 0, &scoped_rows.to_string(), "").is_err());
}

fn reject_invalid_payloads(query: &str, assertion: &Assertion) {
    let wrong_mode = positive_database("phase4-corpus");
    replace_payload_node(&wrong_mode, 1, OPT, directory_node(0o751));
    reject_query_result(query, assertion, &wrong_mode, "wrong directory mode");

    let missing_path = positive_database("phase4-corpus");
    missing_path
        .execute("DELETE FROM files WHERE path = ?1", [STATE])
        .expect("remove required directory payload row");
    reject_query_result(query, assertion, &missing_path, "missing required path");

    let wrong_kind = positive_database("phase4-corpus");
    replace_payload_node(&wrong_kind, 1, STATE, symlink_node("phase4-corpus"));
    reject_query_result(query, assertion, &wrong_kind, "wrong payload kind");

    let wrong_target = positive_database("phase4-corpus-alt");
    reject_query_result(query, assertion, &wrong_target, "wrong symlink target");

    let wrong_path = positive_database("phase4-corpus");
    wrong_path
        .execute(
            "UPDATE files SET path = '/usr/bin/phase4-corpus-link-alt' WHERE path = ?1",
            [LINK],
        )
        .expect("rename required symlink payload row");
    reject_query_result(query, assertion, &wrong_path, "wrong symlink path");
}

fn reject_query_result(query: &str, assertion: &Assertion, database: &Connection, defect: &str) {
    let actual = execute_json_query(database, query);
    assert!(
        evaluate_assertion(assertion, 0, &actual.to_string(), "").is_err(),
        "must reject {defect}: {actual}"
    );
}

fn reject_wrong_sql_alias(query: &str, assertion: &Assertion) {
    let wrong_alias_query = query.replace(" AS kind,", " AS payload_kind,");
    assert_ne!(wrong_alias_query, query, "query must expose the kind alias");
    let actual = execute_json_query(&positive_database("phase4-corpus"), &wrong_alias_query);
    assert!(actual[0].get("payload_kind").is_some());
    assert!(actual[0].get("kind").is_none());
    assert!(
        evaluate_assertion(assertion, 0, &actual.to_string(), "").is_err(),
        "root JSON equality must reject the query's actual wrong alias"
    );
}

fn reject_invalid_results(assertion: &Assertion, expected: &Value) {
    for (row, field, value, defect) in [
        (0, "path", json!("/wrong"), "wrong path value"),
        (0, "kind", json!("symlink"), "wrong kind value"),
        (0, "directory_mode", json!(16873), "wrong directory mode"),
        (
            1,
            "symlink_target",
            json!("phase4-corpus-alt"),
            "wrong symlink target",
        ),
    ] {
        let mut actual = expected.clone();
        actual[row][field] = value;
        reject_rows(assertion, &actual, defect);
    }

    for (row, field, value, defect) in [
        (0, "path", json!(1), "nonstring path"),
        (0, "kind", json!(1), "nonstring kind"),
        (0, "directory_mode", json!("16872"), "string directory mode"),
        (1, "symlink_target", json!(1), "nonstring symlink target"),
        (
            1,
            "directory_mode",
            json!(0),
            "non-null symlink directory mode",
        ),
    ] {
        let mut actual = expected.clone();
        actual[row][field] = value;
        reject_rows(assertion, &actual, defect);
    }

    let mut wrong_key = expected.clone();
    let row = wrong_key[0].as_object_mut().unwrap();
    let mode = row.remove("directory_mode").unwrap();
    row.insert("mode".into(), mode);
    reject_rows(assertion, &wrong_key, "wrong JSON key");

    let mut missing_key = expected.clone();
    missing_key[1]
        .as_object_mut()
        .unwrap()
        .remove("symlink_target");
    reject_rows(assertion, &missing_key, "missing JSON key");

    let mut extra_key = expected.clone();
    extra_key[0]["unexpected"] = json!(true);
    reject_rows(assertion, &extra_key, "extra JSON key");
    reject_rows(assertion, &json!([]), "missing row");
    let mut extra_row = expected.clone();
    extra_row.as_array_mut().unwrap().push(expected[0].clone());
    reject_rows(assertion, &extra_row, "extra row");

    let valid = expected.to_string();
    let trailing = format!("{valid} trailing");
    let concatenated = format!("{valid}{valid}");
    for stdout in ["not JSON", trailing.as_str(), concatenated.as_str()] {
        assert!(
            evaluate_assertion(assertion, 0, stdout, "").is_err(),
            "must reject malformed or non-single JSON output: {stdout}"
        );
    }
    assert!(evaluate_assertion(assertion, 1, &valid, "").is_err());
}

fn reject_rows(assertion: &Assertion, rows: &Value, defect: &str) {
    assert!(
        evaluate_assertion(assertion, 0, &rows.to_string(), "").is_err(),
        "must reject {defect}: {rows}"
    );
}

fn prove_legacy_false_positive(query: &str, assertion: &Assertion) {
    let database = positive_database("phase4-corpus-alt");
    let legacy_stdout = execute_legacy_query(&database, LEGACY_QUERY);
    let legacy_assertion = legacy_text_assertion();
    assert!(evaluate_assertion(&legacy_assertion, 0, &legacy_stdout, "").is_ok());

    let actual = execute_json_query(&database, query);
    assert_eq!(actual[1]["symlink_target"], json!("phase4-corpus-alt"));
    assert!(evaluate_assertion(assertion, 0, &actual.to_string(), "").is_err());
}

fn prove_corrupt_mode_type_false_positive(query: &str, assertion: &Assertion) {
    let database = positive_database("phase4-corpus");
    let encoded: String = database
        .query_row(
            "SELECT payload_node_json FROM files WHERE path = ?1",
            [OPT],
            |row| row.get(0),
        )
        .expect("read validated directory payload before corrupting the negative fixture");
    let mut payload: Value =
        serde_json::from_str(&encoded).expect("parse complete validated payload JSON");
    // This isolated negative models ill-typed persisted state. Positive rows above remain validated.
    payload["source"]["mode"] = json!("16872");
    database
        .execute(
            "UPDATE files SET payload_node_json = ?1 WHERE path = ?2",
            params![payload.to_string(), OPT],
        )
        .expect("store corrupt string mode in negative fixture");

    let actual = execute_json_query(&database, query);
    assert_eq!(actual[0]["directory_mode"], json!("16872"));
    assert!(evaluate_assertion(assertion, 0, &actual.to_string(), "").is_err());

    let legacy_stdout = execute_legacy_query(&database, LEGACY_QUERY);
    assert!(
        evaluate_assertion(&legacy_text_assertion(), 0, &legacy_stdout, "").is_ok(),
        "legacy printf coercion accepts a stored JSON string containing the expected mode"
    );
}

fn legacy_text_assertion() -> Assertion {
    Assertion {
        stdout_contains_all: Some(vec![
            "/opt|directory|0750".into(),
            "/usr/lib/phase4-corpus/state|directory|0750".into(),
            "/usr/bin/phase4-corpus-link|symlink|phase4-corpus".into(),
        ]),
        ..Assertion::default()
    }
}

fn execute_legacy_query(database: &Connection, query: &str) -> String {
    query
        .split(';')
        .filter(|statement| !statement.trim().is_empty())
        .flat_map(|statement| {
            let mut statement = database
                .prepare(statement)
                .expect("prepare legacy SQL extracted from the former manifest step");
            statement
                .query_map([], |row| row.get::<_, String>(0))
                .expect("execute legacy SQL extracted from the former manifest step")
                .collect::<rusqlite::Result<Vec<_>>>()
                .expect("read legacy manifest SQL rows")
        })
        .collect::<Vec<_>>()
        .join("\n")
}
