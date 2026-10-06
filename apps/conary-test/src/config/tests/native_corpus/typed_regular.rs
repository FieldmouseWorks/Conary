// apps/conary-test/src/config/tests/native_corpus/typed_regular.rs
#![cfg(test)]

use super::super::{load_manifest, remi_manifest_path};
use crate::config::manifest::{Assertion, JsonExpectation};
use crate::engine::{
    assertions::evaluate_assertion,
    variables::{expand_assertion, expand_variables},
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

const QUERY: &str = "SELECT path, json_extract(payload_node_json, '$.source.kind.type') AS kind, content_size, content_sha256 FROM files WHERE trove_id = (SELECT id FROM troves WHERE name = 'phase4-daily-driver-corpus') AND json_extract(payload_node_json, '$.source.kind.type') = 'regular' ORDER BY path";
const LEGACY_COUNT: &str = "SELECT COUNT(*) || ' regular files' FROM files WHERE trove_id = (SELECT id FROM troves WHERE name = 'phase4-daily-driver-corpus') AND json_extract(payload_node_json, '$.source.kind.type') = 'regular'";
const LEGACY_ROWS: &str = "SELECT path || '|' || coalesce(content_size, '-') || '|' || json_extract(payload_node_json, '$.source.kind.type') FROM files WHERE trove_id = (SELECT id FROM troves WHERE name = 'phase4-daily-driver-corpus') AND json_extract(payload_node_json, '$.source.kind.type') = 'regular' ORDER BY path";
mod fixtures;
use fixtures::{
    ANCHOR, COPY, HARDLINK_SHA, TRACKED, database, expected_rows, insert_file,
    verify_pinned_fixture_bytes,
};

const LANES: &[(&str, &str)] = &[
    ("fedora44", COPY),
    ("ubuntu-26.04", ANCHOR),
    ("arch", ANCHOR),
];
#[test]
fn native_corpus_tnpm15_regular_rows_require_exact_scoped_json() {
    verify_pinned_fixture_bytes();
    let manifest = load_manifest(&remi_manifest_path(
        "phase4-native-daily-driver-corpus.toml",
    ))
    .expect("load native daily-driver corpus manifest");
    let test = manifest
        .test
        .iter()
        .find(|test| test.id == "TNPM15")
        .expect("TNPM15");
    assert_eq!(test.step.len(), 10);
    let step = &test.step[1];
    let command = step.run.as_deref().expect("regular-file SQL command");
    assert_eq!(command, format!("sqlite3 -json ${{DB_PATH}} \"{QUERY}\""));
    assert_eq!(command.matches("sqlite3").count(), 1);
    assert_eq!(QUERY.matches("SELECT ").count(), 2); // outer row query and trove identity
    assert!(!QUERY.contains(';'));
    let assertion = step.assert.as_ref().expect("regular-file assertion");
    assert_eq!(assertion.exit_code, Some(0));
    assert!(assertion.exit_code_not.is_none());
    assert!(assertion.stdout_contains_all.is_none());
    assert!(assertion.stdout_contains.is_none());
    assert!(assertion.stdout_contains_any.is_none());
    assert!(assertion.stdout_contains_if_success.is_none());
    assert!(assertion.stdout_contains_any_if_success.is_none());
    let [root] = assertion.stdout_json.as_deref().expect("one JSON check") else {
        panic!("regular-file proof must compare one document");
    };
    assert_eq!(root.pointer, "");

    for &(distro, regular_hardlink) in LANES {
        let overrides = manifest
            .distro_overrides
            .get(distro)
            .expect("lane overrides");
        assert_eq!(
            overrides["native_corpus_regular_hardlink_path"],
            regular_hardlink
        );
        assert_eq!(expand_variables(command, overrides), command);
        let expanded = expand_assertion(assertion, overrides);
        let expected = expected_rows(regular_hardlink);
        assert_eq!(expected.as_array().unwrap().len(), 9, "{distro} row count");
        assert_eq!(
            expanded.stdout_json.as_ref().unwrap()[0].expected,
            JsonExpectation::Equals(expected.clone()),
            "{distro} literal fixture pins"
        );
        let (_directory, db) = database(regular_hardlink);
        let actual = query_rows(&db);
        assert_eq!(
            actual, expected,
            "{distro} production-schema SQL rows and order"
        );
        assert!(evaluate_assertion(&expanded, 0, &actual.to_string(), "").is_ok());
        reject_database_defects(&db, &expanded);
        reject_output_defects(&expanded, &expected);
    }

    let (_directory, fedora) = database(COPY);
    fedora.execute("UPDATE files SET path = '/usr/bin/phase4-corpus-corrupt' WHERE path = '/usr/bin/phase4-corpus'", []).unwrap();
    let old_stdout = legacy_stdout(&fedora);
    let old_assertion = Assertion {
        exit_code: Some(0),
        stdout_contains_all: Some(vec![
            "9 regular files".into(),
            "/etc/phase4-corpus/app.conf".into(),
            "/etc/phase4-corpus/app-deleted.conf".into(),
            "/etc/phase4-corpus/app-local.conf".into(),
            "/usr/bin/phase4-corpus-alt".into(),
            "/usr/lib/kernel/install.d/95-phase4-corpus.install".into(),
            "/usr/lib/systemd/system/phase4-corpus.service".into(),
            "/usr/share/phase4-corpus/large-payload.bin|2097152|regular".into(),
        ]),
        ..Assertion::default()
    };
    assert!(
        evaluate_assertion(&old_assertion, 0, &old_stdout, "").is_ok(),
        "old text check passes the wrong path"
    );
    reject(
        assertion,
        &query_rows(&fedora),
        "old-pass wrong-path witness",
    );
}

fn query_rows(db: &Connection) -> Value {
    let mut statement = db.prepare(QUERY).expect("prepare loaded manifest SQL");
    let rows = statement.query_map([], |r| {
        Ok(json!({"path": r.get::<_, String>(0)?, "kind": r.get::<_, Option<String>>(1)?,
            "content_size": r.get::<_, Option<i64>>(2)?, "content_sha256": r.get::<_, Option<String>>(3)?}))
    }).expect("execute loaded manifest SQL")
        .collect::<rusqlite::Result<Vec<_>>>().expect("read loaded manifest rows");
    Value::Array(rows)
}

fn reject_database_defects(db: &Connection, assertion: &Assertion) {
    let path = "/usr/bin/phase4-corpus";
    defect(
        db,
        assertion,
        "UPDATE files SET content_sha256='bad-digest' WHERE path='/usr/bin/phase4-corpus'",
        "wrong persisted SHA-256",
    );
    db.execute(
        "UPDATE files SET content_sha256=?1 WHERE path=?2",
        params![TRACKED[3].2, path],
    )
    .unwrap();
    defect(
        db,
        assertion,
        "UPDATE files SET content_size=49 WHERE path='/usr/bin/phase4-corpus'",
        "wrong persisted size",
    );
    db.execute_batch("UPDATE files SET content_size=48 WHERE path='/usr/bin/phase4-corpus'")
        .unwrap();
    defect(
        db,
        assertion,
        "UPDATE files SET payload_node_json=json_set(payload_node_json,'$.source.kind.type','directory') WHERE path='/usr/bin/phase4-corpus'",
        "wrong persisted kind",
    );
    db.execute_batch("UPDATE files SET payload_node_json=json_set(payload_node_json,'$.source.kind.type','regular') WHERE path='/usr/bin/phase4-corpus'").unwrap();
    defect(
        db,
        assertion,
        "DELETE FROM files WHERE path='/usr/bin/phase4-corpus'",
        "missing regular row",
    );
    insert_file(db, 1, path, "regular", Some((48, TRACKED[3].2)), None);
    insert_file(
        db,
        1,
        "/usr/share/phase4-corpus/extra",
        "regular",
        Some((1, HARDLINK_SHA)),
        None,
    );
    reject(assertion, &query_rows(db), "extra regular row and count");
}

fn defect(db: &Connection, assertion: &Assertion, sql: &str, label: &str) {
    db.execute_batch(sql).expect("mutate schema-valid witness");
    reject(assertion, &query_rows(db), label);
}

fn reject_output_defects(assertion: &Assertion, expected: &Value) {
    let mut missing_key = expected.clone();
    missing_key[0]
        .as_object_mut()
        .unwrap()
        .remove("content_sha256");
    reject(assertion, &missing_key, "missing key");
    let mut extra_key = expected.clone();
    extra_key[0]["unexpected"] = json!(true);
    reject(assertion, &extra_key, "extra key");
    for (field, value) in [
        ("path", json!("/usr/bin/phase4-corpus-corrupt")),
        ("kind", json!("directory")),
        ("kind", json!(null)),
        ("kind", json!(7)),
        ("content_size", json!("22")),
        ("content_size", json!(22.0)),
        ("content_size", json!(true)),
        ("content_size", json!(null)),
        ("content_sha256", json!(null)),
    ] {
        let mut changed = expected.clone();
        changed[0][field] = value;
        reject(assertion, &changed, "wrong field or type");
    }
    let mut wrong_order = expected.clone();
    wrong_order.as_array_mut().unwrap().swap(0, 1);
    reject(assertion, &wrong_order, "wrong row order");
    reject(assertion, &json!([]), "empty result");
    reject(assertion, &json!({}), "wrong root shape");
    let valid = expected.to_string();
    for stdout in [
        "malformed",
        &format!("{valid} trailing"),
        &format!("{valid}{valid}"),
    ] {
        assert!(
            evaluate_assertion(assertion, 0, stdout, "").is_err(),
            "reject malformed output: {stdout}"
        );
    }
    assert!(
        evaluate_assertion(assertion, 1, &valid, "").is_err(),
        "reject nonzero command"
    );
}

fn legacy_stdout(db: &Connection) -> String {
    let count: String = db.query_row(LEGACY_COUNT, [], |r| r.get(0)).unwrap();
    let mut statement = db.prepare(LEGACY_ROWS).unwrap();
    let rows = statement
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    format!("{count}\n{}", rows.join("\n"))
}

fn reject(assertion: &Assertion, rows: &Value, defect: &str) {
    assert!(
        evaluate_assertion(assertion, 0, &rows.to_string(), "").is_err(),
        "must reject {defect}: {rows}"
    );
}
