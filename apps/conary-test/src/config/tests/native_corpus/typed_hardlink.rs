// apps/conary-test/src/config/tests/native_corpus/typed_hardlink.rs
#![cfg(test)]

use super::super::{load_manifest, remi_manifest_path};
use crate::config::manifest::{Assertion, JsonExpectation, TestStep};
use crate::engine::{assertions::evaluate_assertion, variables::expand_variables};
use conary_core::payload::{
    PayloadContentAuthority, PayloadNode, PayloadNodeKind, ResolvedPayloadNode,
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

const QUERY: &str = r#"WITH scoped AS (SELECT path, json_extract(payload_node_json, '$.source.kind.type') AS kind FROM files WHERE trove_id = (SELECT id FROM troves WHERE name = 'phase4-daily-driver-corpus')) SELECT COUNT(CASE WHEN kind = 'hardlink' THEN 1 END) AS hardlink_count, COUNT(CASE WHEN path = '/usr/lib/phase4-corpus/hardlink-anchor' AND kind IN ('regular','hardlink') THEN 1 END) AS anchor_member, COUNT(CASE WHEN path = '/usr/lib/phase4-corpus/hardlink-copy' AND kind IN ('regular','hardlink') THEN 1 END) AS copy_member, COUNT(CASE WHEN path IN ('/usr/lib/phase4-corpus/hardlink-anchor','/usr/lib/phase4-corpus/hardlink-copy') AND kind = 'hardlink' THEN 1 END) AS named_hardlink_count FROM scoped"#;
const LEGACY_QUERY: &str = r#"SELECT COUNT(*) || ' hardlinks' FROM files WHERE trove_id = (SELECT id FROM troves WHERE name = 'phase4-daily-driver-corpus') AND json_extract(payload_node_json, '$.source.kind.type') = 'hardlink'; SELECT path || '|' || json_extract(payload_node_json, '$.source.kind.type') FROM files WHERE trove_id = (SELECT id FROM troves WHERE name = 'phase4-daily-driver-corpus') AND path IN ('/usr/lib/phase4-corpus/hardlink-anchor', '/usr/lib/phase4-corpus/hardlink-copy') ORDER BY path"#;
const ANCHOR: &str = "/usr/lib/phase4-corpus/hardlink-anchor";
const COPY: &str = "/usr/lib/phase4-corpus/hardlink-copy";
const LANES: &[(&str, &str, &str)] = &[
    ("fedora44", "hardlink", "regular"),
    ("ubuntu-26.04", "regular", "hardlink"),
    ("arch", "regular", "hardlink"),
];

#[test]
fn native_corpus_tnpm15_hardlinks_require_exact_scoped_json_counts() {
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

    let step = &test.step[3];
    let command = step.run.as_deref().expect("TNPM15 hardlink query");
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

    for &(distro, anchor_kind, copy_kind) in LANES {
        let overrides = manifest
            .distro_overrides
            .get(distro)
            .unwrap_or_else(|| panic!("missing {distro} overrides"));
        assert_eq!(expand_variables(command, overrides), expected_command);

        let database = positive_database(anchor_kind, copy_kind);
        let unrelated_count: i64 = database
            .query_row(
                "SELECT COUNT(*) FROM files WHERE trove_id = 2 AND json_extract(payload_node_json, '$.source.kind.type') = 'hardlink'",
                [],
                |row| row.get(0),
            )
            .expect("count unrelated-trove hardlinks");
        assert_eq!(unrelated_count, 3);
        let actual = execute_json_query(&database, query);
        assert_eq!(actual, expected, "{distro} orientation and trove scope");
        assert!(evaluate_assertion(assertion, 0, &actual.to_string(), "").is_ok());
    }

    reject_semantically_invalid_topologies(assertion);
    reject_invalid_results(assertion, &expected);
    prove_legacy_false_positives(assertion);
}

fn sqlite_query(step: &TestStep) -> &str {
    step.run
        .as_deref()
        .and_then(|command| command.strip_prefix("sqlite3 -json ${DB_PATH} \""))
        .and_then(|query| query.strip_suffix('"'))
        .expect("TNPM15 hardlink query must be one quoted sqlite3 JSON command")
}

fn typed_root_assertion(step: &TestStep) -> &Assertion {
    let assertion = step.assert.as_ref().expect("hardlink step assertion");
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
    json!([{
        "hardlink_count": 1,
        "anchor_member": 1,
        "copy_member": 1,
        "named_hardlink_count": 1
    }])
}

fn positive_database(anchor_kind: &str, copy_kind: &str) -> Connection {
    let database = database();
    insert_named_pair(&database, 1, anchor_kind, copy_kind);
    for index in 0..3 {
        insert_hardlink_group(
            &database,
            2,
            &format!("/usr/share/unrelated-hardlinks/{index}-target"),
            &format!("/usr/share/unrelated-hardlinks/{index}"),
            &format!("fixture:unrelated:{index}"),
        );
    }
    database
}

fn database() -> Connection {
    let database = Connection::open_in_memory().expect("open in-memory SQLite database");
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
        .expect("create hardlink fixture schema");
    database
}

fn insert_named_pair(database: &Connection, trove_id: i64, anchor: &str, copy: &str) {
    let identity = format!("fixture:named:{trove_id}");
    match (anchor, copy) {
        ("hardlink", "regular") => {
            insert_hardlink_group(database, trove_id, COPY, ANCHOR, &identity)
        }
        ("regular", "hardlink") => {
            insert_hardlink_group(database, trove_id, ANCHOR, COPY, &identity)
        }
        _ => panic!("unsupported named hardlink orientation: {anchor}|{copy}"),
    }
}

fn insert_hardlink_group(
    database: &Connection,
    trove_id: i64,
    target_path: &str,
    alias_path: &str,
    identity: &str,
) {
    let mut regular = PayloadNode::regular(0o644);
    regular.kind = PayloadNodeKind::Regular {
        hardlink_identity: Some(identity.to_owned()),
    };
    insert_payload_node(
        database,
        trove_id,
        target_path,
        regular,
        Some(b"fixture payload"),
    );

    let alias = PayloadNode {
        kind: PayloadNodeKind::Hardlink {
            target: target_path.to_owned(),
            identity: identity.to_owned(),
        },
        ..PayloadNode::regular(0o644)
    };
    insert_payload_node(database, trove_id, alias_path, alias, None);
}

fn insert_regular(database: &Connection, trove_id: i64, path: &str) {
    insert_payload_node(
        database,
        trove_id,
        path,
        PayloadNode::regular(0o644),
        Some(b"fixture payload"),
    );
}

fn insert_payload_node(
    database: &Connection,
    trove_id: i64,
    path: &str,
    source: PayloadNode,
    content: Option<&[u8]>,
) {
    let node = ResolvedPayloadNode::from_numeric_source(source)
        .expect("fixture source node resolves numeric ownership");
    let content = content.map(|bytes| PayloadContentAuthority {
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
        .expect("insert fixture file row");
}

fn overwrite_kind_type(database: &Connection, path: &str, kind: Value) {
    let encoded: String = database
        .query_row(
            "SELECT payload_node_json FROM files WHERE path = ?1",
            [path],
            |row| row.get(0),
        )
        .expect("read valid node before injecting bad kind");
    let mut payload: Value = serde_json::from_str(&encoded).expect("parse serialized node");
    payload["source"]["kind"]["type"] = kind;
    database
        .execute(
            "UPDATE files SET payload_node_json = ?1 WHERE path = ?2",
            params![payload.to_string(), path],
        )
        .expect("inject isolated raw kind defect");
}

fn execute_json_query(database: &Connection, query: &str) -> Value {
    let mut statement = database
        .prepare(query)
        .expect("prepare SQL extracted from the manifest");
    let rows = statement
        .query_map([], |row| {
            Ok(json!({
                "hardlink_count": row.get::<_, i64>(0)?,
                "anchor_member": row.get::<_, i64>(1)?,
                "copy_member": row.get::<_, i64>(2)?,
                "named_hardlink_count": row.get::<_, i64>(3)?,
            }))
        })
        .expect("execute SQL extracted from the manifest")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("read manifest SQL rows");
    Value::Array(rows)
}

fn reject_semantically_invalid_topologies(assertion: &Assertion) {
    let missing_member = database();
    insert_regular(&missing_member, 1, ANCHOR);
    reject_query_result(assertion, &missing_member, "missing named member");

    let both_hardlink = database();
    let target = "/usr/lib/phase4-corpus/hardlink-target";
    let identity = "fixture:both-hardlinks";
    let mut regular = PayloadNode::regular(0o644);
    regular.kind = PayloadNodeKind::Regular {
        hardlink_identity: Some(identity.to_owned()),
    };
    insert_payload_node(&both_hardlink, 1, target, regular, Some(b"fixture payload"));
    for path in [ANCHOR, COPY] {
        let alias = PayloadNode {
            kind: PayloadNodeKind::Hardlink {
                target: target.to_owned(),
                identity: identity.to_owned(),
            },
            ..PayloadNode::regular(0o644)
        };
        insert_payload_node(&both_hardlink, 1, path, alias, None);
    }
    reject_query_result(assertion, &both_hardlink, "both named rows hardlinks");

    for kind in [Value::Null, json!("future-kind")] {
        let invalid_kind = database();
        insert_named_pair(&invalid_kind, 1, "regular", "hardlink");
        overwrite_kind_type(&invalid_kind, ANCHOR, kind);
        reject_query_result(assertion, &invalid_kind, "null or unknown named kind");
    }
}

fn reject_query_result(assertion: &Assertion, database: &Connection, defect: &str) {
    let actual = execute_json_query(database, QUERY);
    assert!(
        evaluate_assertion(assertion, 0, &actual.to_string(), "").is_err(),
        "must reject {defect}: {actual}"
    );
}

fn reject_invalid_results(assertion: &Assertion, expected: &Value) {
    let mut wrong_field = expected.clone();
    let object = wrong_field[0].as_object_mut().unwrap();
    let value = object.remove("anchor_member").unwrap();
    object.insert("anchor_exists".into(), value);
    reject_rows(assertion, &wrong_field, "wrong field");

    let mut missing_field = expected.clone();
    missing_field[0]
        .as_object_mut()
        .unwrap()
        .remove("copy_member");
    reject_rows(assertion, &missing_field, "missing field");

    let mut extra_field = expected.clone();
    extra_field[0]["unexpected"] = json!(true);
    reject_rows(assertion, &extra_field, "extra field");

    let mut wrong_value = expected.clone();
    wrong_value[0]["named_hardlink_count"] = json!(0);
    reject_rows(assertion, &wrong_value, "wrong field value");
    reject_rows(assertion, &json!([]), "missing row");
    reject_rows(
        assertion,
        &json!([expected[0].clone(), expected[0].clone()]),
        "extra row",
    );

    for value in [json!("1"), json!(1.0), json!(true), Value::Null] {
        let mut wrong_type = expected.clone();
        wrong_type[0]["hardlink_count"] = value;
        reject_rows(assertion, &wrong_type, "noninteger hardlink count");
    }

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

fn prove_legacy_false_positives(assertion: &Assertion) {
    let legacy = legacy_assertion();
    for (database, count, named_count) in [
        (legacy_database("hardlink", "regular", 10, 0), 11, 1),
        (legacy_database("regular", "regular", 1, 3), 1, 0),
    ] {
        let legacy_output = execute_legacy_query(&database);
        assert!(legacy_output.starts_with(&format!("{count} hardlinks\n")));
        assert!(evaluate_assertion(&legacy, 0, &legacy_output, "").is_ok());
        let actual = execute_json_query(&database, QUERY);
        assert_eq!(actual[0]["hardlink_count"], json!(count));
        assert_eq!(actual[0]["named_hardlink_count"], json!(named_count));
        assert!(evaluate_assertion(assertion, 0, &actual.to_string(), "").is_err());
    }
}

fn legacy_database(
    anchor: &str,
    copy: &str,
    extra_hardlinks: usize,
    unrelated: usize,
) -> Connection {
    let database = database();
    match (anchor, copy) {
        ("hardlink", "regular") | ("regular", "hardlink") => {
            insert_named_pair(&database, 1, anchor, copy);
        }
        ("regular", "regular") => {
            insert_regular(&database, 1, ANCHOR);
            insert_regular(&database, 1, COPY);
        }
        _ => panic!("unsupported named hardlink orientation: {anchor}|{copy}"),
    }
    for index in 0..extra_hardlinks {
        insert_hardlink_group(
            &database,
            1,
            &format!("/usr/lib/phase4-corpus/extra-hardlink-target-{index}"),
            &format!("/usr/lib/phase4-corpus/extra-hardlink-{index}"),
            &format!("fixture:extra:{index}"),
        );
    }
    for index in 0..unrelated {
        insert_hardlink_group(
            &database,
            2,
            &format!("/usr/share/unrelated-hardlinks/{index}-target"),
            &format!("/usr/share/unrelated-hardlinks/{index}"),
            &format!("fixture:unrelated:{index}"),
        );
    }
    database
}

fn legacy_assertion() -> Assertion {
    Assertion {
        exit_code: Some(0),
        stdout_contains_all: Some(vec![
            "1 hardlinks".into(),
            format!("{ANCHOR}|"),
            format!("{COPY}|"),
        ]),
        ..Assertion::default()
    }
}

fn execute_legacy_query(database: &Connection) -> String {
    let mut statements = LEGACY_QUERY.split(';');
    let count_query = statements.next().expect("legacy hardlink count query");
    let named_query = statements.next().expect("legacy named-file query");
    assert!(
        statements.next().is_none(),
        "legacy control has two queries"
    );
    let count: String = database
        .query_row(count_query, [], |row| row.get(0))
        .expect("execute legacy hardlink count SQL");
    let mut statement = database
        .prepare(named_query)
        .expect("prepare legacy named-file SQL");
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .expect("execute legacy named-file SQL")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("read legacy named-file rows");
    format!("{count}\n{}", rows.join("\n"))
}
