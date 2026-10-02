// apps/conary-test/src/config/tests/native_corpus/typed_installed.rs
#![cfg(test)]

use super::super::{load_manifest, remi_manifest_path};
use crate::config::manifest::{Assertion, JsonExpectation};
use crate::engine::{assertions::evaluate_assertion, variables::expand_assertion};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

const QUERY: &str = "SELECT name, version, COALESCE(architecture, '') AS architecture, COALESCE(version_scheme, '') AS version_scheme, COALESCE(source_profile, '') AS source_profile, COALESCE(install_source, '') AS install_source, COALESCE(install_reason, '') AS install_reason FROM troves WHERE name = 'phase4-daily-driver-corpus' ORDER BY id";
const ADJACENT_RUN: &str = r#"sqlite3 ${DB_PATH} "SELECT COUNT(*) || ' regular files' FROM files WHERE trove_id = (SELECT id FROM troves WHERE name = 'phase4-daily-driver-corpus') AND json_extract(payload_node_json, '$.source.kind.type') = 'regular'; SELECT path || '|' || coalesce(content_size, '-') || '|' || json_extract(payload_node_json, '$.source.kind.type') FROM files WHERE trove_id = (SELECT id FROM troves WHERE name = 'phase4-daily-driver-corpus') AND json_extract(payload_node_json, '$.source.kind.type') = 'regular' ORDER BY path""#;
const PRE_CONFIG_ORDER: &[&str] = &[
    "path IN ('/opt', '/usr/bin/phase4-corpus-link', '/usr/lib/phase4-corpus/state')",
    "COUNT(CASE WHEN kind = 'hardlink' THEN 1 END) AS hardlink_count",
    "COUNT(*) || ' requirement groups'",
];
const POST_CONFIG_ORDER: &[&str] = &[
    "FROM provides",
    "FROM installed_native_lifecycle_bundles",
    "FROM activation_requests",
];
const LANES: &[(&str, &str, &str, &str, &str, &str)] = &[
    ("fedora44", "rpm", "x86_64", "rpm", "fedora-44", "1.0.0-1"),
    (
        "ubuntu-26.04",
        "deb",
        "amd64",
        "debian",
        "ubuntu-26.04",
        "1.0.0-1",
    ),
    ("arch", "arch", "x86_64", "arch", "arch", "1.0.0-1"),
];

#[test]
fn native_corpus_tnpm15_requires_exact_installed_trove_identity() {
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
    assert_eq!(test.step[1].run.as_deref(), Some(ADJACENT_RUN));
    for (index, (step, signature)) in test.step[2..5].iter().zip(PRE_CONFIG_ORDER).enumerate() {
        let command = step.run.as_deref().expect("ordered TNPM15 command");
        let expected_prefix = if index < 2 {
            "sqlite3 -json ${DB_PATH} \""
        } else {
            "sqlite3 ${DB_PATH} \""
        };
        assert!(
            command.starts_with(expected_prefix),
            "TNPM15 step {} must use {expected_prefix}",
            index + 2
        );
        assert!(
            command.contains(signature),
            "TNPM15 command order: {signature}"
        );
    }
    for (step, signature) in test.step[6..9].iter().zip(POST_CONFIG_ORDER) {
        let command = step.run.as_deref().expect("ordered TNPM15 command");
        assert!(command.starts_with("sqlite3 ${DB_PATH} \""));
        assert!(
            command.contains(signature),
            "TNPM15 command order: {signature}"
        );
    }

    let metadata = &test.step[0];
    let command = metadata.run.as_deref().expect("installed identity query");
    assert_eq!(command, format!("sqlite3 -json ${{DB_PATH}} \"{QUERY}\""));
    assert_eq!(command.matches("sqlite3").count(), 1);
    assert_eq!(command.matches("SELECT").count(), 1);
    assert!(!QUERY.contains(';'));
    let query = command
        .strip_prefix("sqlite3 -json ${DB_PATH} \"")
        .and_then(|query| query.strip_suffix('"'))
        .expect("one quoted sqlite3 JSON query");

    let assertion = metadata.assert.as_ref().expect("identity assertion");
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
    let checks = assertion.stdout_json.as_ref().expect("root JSON equality");
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].pointer, "");

    for &(distro, target, architecture, scheme, profile, version) in LANES {
        let overrides = manifest
            .distro_overrides
            .get(distro)
            .expect("lane overrides");
        for (key, value) in [
            ("native_target", target),
            ("native_arch", architecture),
            ("native_scheme", scheme),
            ("native_profile", profile),
            ("native_corpus_fixture_version", version),
        ] {
            assert_eq!(
                overrides.get(key).map(String::as_str),
                Some(value),
                "{distro} {key}"
            );
        }
        let expanded = expand_assertion(assertion, overrides);
        let expected = expected_rows(architecture, scheme, profile, version);
        assert!(
            matches!(&expanded.stdout_json.as_ref().unwrap()[0].expected, JsonExpectation::Equals(value) if value == &expected)
        );
        assert!(evaluate_assertion(&expanded, 0, &expected.to_string(), "").is_ok());
        reject_invalid_results(&expanded, &expected);
        prove_manifest_sql(
            query,
            &expanded,
            &expected,
            architecture,
            scheme,
            profile,
            version,
        );
    }

    let expected = expected_rows("x86_64", "rpm", "fedora-44", "1.0.0-1");
    let identity = [
        "name",
        "version",
        "architecture",
        "version_scheme",
        "source_profile",
        "install_source",
        "install_reason",
    ]
    .map(|field| expected[0][field].as_str().unwrap())
    .join("|");
    let legacy = Assertion {
        stdout_contains_all: Some(vec!["1 troves".into(), identity.clone()]),
        ..Assertion::default()
    };
    let stdout = format!("11 troves\n{identity}");
    assert!(evaluate_assertion(&legacy, 0, &stdout, "").is_ok());
    assert!(evaluate_assertion(assertion, 0, &stdout, "").is_err());
}

fn expected_rows(architecture: &str, scheme: &str, profile: &str, version: &str) -> Value {
    json!([{"name":"phase4-daily-driver-corpus", "version":version, "architecture":architecture,
        "version_scheme":scheme, "source_profile":profile, "install_source":"file", "install_reason":"explicit"}])
}

fn reject_invalid_results(assertion: &Assertion, expected: &Value) {
    for field in [
        "name",
        "version",
        "architecture",
        "version_scheme",
        "source_profile",
        "install_source",
        "install_reason",
    ] {
        for value in [json!("wrong-value"), json!(1)] {
            let mut wrong = expected.clone();
            wrong[0][field] = value;
            must_reject(assertion, &wrong);
        }
        let mut missing_key = expected.clone();
        missing_key[0].as_object_mut().unwrap().remove(field);
        must_reject(assertion, &missing_key);
    }
    let mut extra_key = expected.clone();
    extra_key[0]["unexpected"] = json!(true);
    must_reject(assertion, &extra_key);
    must_reject(assertion, &json!([]));
    must_reject(
        assertion,
        &json!([expected[0].clone(), expected[0].clone()]),
    );
    let mut extra_row = expected.clone();
    let mut foreign = expected[0].clone();
    foreign["name"] = json!("foreign-package");
    extra_row.as_array_mut().unwrap().push(foreign);
    must_reject(assertion, &extra_row);

    let trailing = format!("{expected} trailing");
    let concatenated = format!("{expected}{expected}");
    for stdout in ["not JSON", trailing.as_str(), concatenated.as_str()] {
        assert!(evaluate_assertion(assertion, 0, stdout, "").is_err());
    }
    assert!(evaluate_assertion(assertion, 1, &expected.to_string(), "").is_err());
}

fn must_reject(assertion: &Assertion, actual: &Value) {
    assert!(evaluate_assertion(assertion, 0, &actual.to_string(), "").is_err());
}

fn prove_manifest_sql(
    query: &str,
    assertion: &Assertion,
    expected: &Value,
    architecture: &str,
    scheme: &str,
    profile: &str,
    version: &str,
) {
    let database = trove_database();
    insert_trove(
        &database,
        1,
        "phase4-daily-driver-corpus",
        version,
        architecture,
        scheme,
        profile,
    );
    insert_trove(
        &database,
        2,
        "foreign-package",
        "8.0",
        architecture,
        scheme,
        profile,
    );
    let target = execute_json_query(&database, query);
    assert_eq!(
        &target, expected,
        "name predicate excludes the foreign package"
    );
    assert!(evaluate_assertion(assertion, 0, &target.to_string(), "").is_ok());

    insert_trove(
        &database,
        3,
        "phase4-daily-driver-corpus",
        "9.9.9",
        architecture,
        scheme,
        profile,
    );
    let duplicate = execute_json_query(&database, query);
    assert_eq!(duplicate.as_array().unwrap().len(), 2);
    must_reject(assertion, &duplicate);
}

fn trove_database() -> Connection {
    let database = Connection::open_in_memory().expect("open in-memory trove database");
    database.execute_batch("CREATE TABLE troves (id INTEGER PRIMARY KEY, name TEXT NOT NULL, version TEXT NOT NULL, architecture TEXT, version_scheme TEXT, source_profile TEXT, install_source TEXT, install_reason TEXT);").expect("create target trove schema");
    database
}

fn insert_trove(
    database: &Connection,
    id: i64,
    name: &str,
    version: &str,
    architecture: &str,
    scheme: &str,
    profile: &str,
) {
    database
        .execute(
            "INSERT INTO troves VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'file', 'explicit')",
            params![id, name, version, architecture, scheme, profile],
        )
        .expect("insert in-memory trove");
}

fn execute_json_query(database: &Connection, query: &str) -> Value {
    let mut statement = database
        .prepare(query)
        .expect("prepare extracted manifest SQL");
    let rows = statement.query_map([], |row| Ok(json!({
        "name": row.get::<_, String>(0)?, "version": row.get::<_, String>(1)?,
        "architecture": row.get::<_, String>(2)?, "version_scheme": row.get::<_, String>(3)?,
        "source_profile": row.get::<_, String>(4)?, "install_source": row.get::<_, String>(5)?,
        "install_reason": row.get::<_, String>(6)?,
    }))).expect("execute extracted manifest SQL").collect::<rusqlite::Result<Vec<_>>>().expect("read SQL rows");
    Value::Array(rows)
}
