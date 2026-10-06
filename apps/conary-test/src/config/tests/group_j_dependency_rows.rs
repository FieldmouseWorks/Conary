// apps/conary-test/src/config/tests/group_j_dependency_rows.rs

#![cfg(test)]

use super::{load_manifest, remi_manifest_path};
use crate::config::manifest::{Assertion, JsonExpectation, TestDef};
use crate::engine::assertions::evaluate_assertion;
use rusqlite::Connection;
use serde_json::{Value, json};

const T110_QUERY: &str = "SELECT name, version, package_release, version_scheme FROM troves WHERE name IN ('dep-app', 'dep-base', 'dep-liba', 'dep-libb') ORDER BY name";
const T113_QUERY: &str =
    "SELECT name, version, package_release, version_scheme FROM troves WHERE name = 'dep-liba'";
const MISMATCH: &str = "did not match expected value";
const NOT_JSON: &str = "stdout is not valid JSON";

fn find_test(manifest: &crate::config::manifest::TestManifest, id: &str) -> TestDef {
    manifest
        .test
        .iter()
        .find(|test| test.id == id)
        .unwrap_or_else(|| panic!("{id} must exist"))
        .clone()
}

/// The single state-query step and assertion of a test, after checking the
/// exact command text and that the assertion is purely typed JSON.
fn row_assertion(test: &TestDef, query: &str, expected: &Value) -> Assertion {
    let steps: Vec<_> = test
        .step
        .iter()
        .filter(|step| {
            step.run
                .as_deref()
                .is_some_and(|run| run.starts_with("sqlite3 "))
        })
        .collect();
    let [step] = steps.as_slice() else {
        panic!("{} must have exactly one sqlite3 row step", test.id);
    };
    assert_eq!(
        step.run.as_deref(),
        Some(format!("sqlite3 -json ${{DB_PATH}} \"{query}\"").as_str())
    );
    let assertion = step.assert.as_ref().expect("row step needs an assertion");
    assert_eq!(assertion.exit_code, Some(0));
    assert!(assertion.stdout_contains.is_none());
    assert!(assertion.stdout_contains_all.is_none());
    assert!(assertion.stdout_contains_any.is_none());
    let [whole] = assertion.stdout_json.as_deref().expect("typed rows") else {
        panic!("{} must have exactly one JSON assertion", test.id);
    };
    assert_eq!(whole.pointer, "");
    assert_eq!(whole.expected, JsonExpectation::Equals(expected.clone()));
    assertion.clone()
}

fn row(name: &str, version: &str) -> Value {
    json!({
        "name": name,
        "version": version,
        "package_release": "1",
        "version_scheme": "conary",
    })
}

fn seeded_database(rows: &[(&str, &str)]) -> Connection {
    let database = Connection::open_in_memory().expect("open disposable SQLite database");
    database
        .execute_batch(
            "CREATE TABLE troves (name TEXT NOT NULL, version TEXT NOT NULL,
             package_release TEXT, version_scheme TEXT NOT NULL);",
        )
        .expect("create troves fixture");
    for (name, version) in rows {
        database
            .execute(
                "INSERT INTO troves VALUES (?1, ?2, '1', 'conary')",
                [name, version],
            )
            .expect("seed trove");
    }
    database
}

/// Run the manifest query and render rows as `sqlite3 -json` does.
fn query_json(database: &Connection, query: &str) -> String {
    let mut statement = database.prepare(query).expect("prepare manifest query");
    let rows: Vec<Value> = statement
        .query_map([], |r| {
            Ok(json!({
                "name": r.get::<_, String>(0)?,
                "version": r.get::<_, String>(1)?,
                "package_release": r.get::<_, String>(2)?,
                "version_scheme": r.get::<_, String>(3)?,
            }))
        })
        .expect("run manifest query")
        .collect::<Result<_, _>>()
        .expect("read rows");
    Value::Array(rows).to_string()
}

fn rejects(assertion: &Assertion, exit_code: i32, stdout: &str, diagnostic: &str) {
    let error = evaluate_assertion(assertion, exit_code, stdout, "")
        .expect_err("negative control must fail")
        .to_string();
    assert!(
        error.contains(diagnostic),
        "wrong failure for {stdout:?}: {error}"
    );
}

fn exercise_controls(assertion: &Assertion, query: &str, installed: &[&str], rows: &[Value]) {
    let seed: Vec<(&str, &str)> = installed.iter().map(|n| (*n, "1.0.0")).collect();
    let mut seeded = seed.clone();
    seeded.extend([("dep-liba-extra", "1.0.0"), ("unrelated", "11.0.0")]);
    let database = seeded_database(&seeded);
    let actual = query_json(&database, query);
    assert_eq!(
        serde_json::from_str::<Value>(&actual).unwrap(),
        Value::Array(rows.to_vec()),
        "query must return exactly the expected rows and ignore other names"
    );
    // Positive control.
    evaluate_assertion(assertion, 0, &actual, "").expect("exact rows pass");

    // Wrong version: the state a replaced dep-liba would leave behind.
    let replaced: Vec<(&str, &str)> = seed
        .iter()
        .map(|(n, v)| {
            if *n == "dep-liba" {
                (*n, "2.0.0")
            } else {
                (*n, *v)
            }
        })
        .collect();
    let replaced_json = query_json(&seeded_database(&replaced), query);
    rejects(assertion, 0, &replaced_json, MISMATCH);
    let lookalike: Vec<(&str, &str)> = seed
        .iter()
        .map(|(n, v)| {
            if *n == "dep-liba" {
                (*n, "11.0.0")
            } else {
                (*n, *v)
            }
        })
        .collect();
    rejects(
        assertion,
        0,
        &query_json(&seeded_database(&lookalike), query),
        MISMATCH,
    );

    // Missing row, extra duplicate row.
    let mut parsed: Vec<Value> = serde_json::from_str(&actual).unwrap();
    let removed = parsed.pop().unwrap();
    rejects(
        assertion,
        0,
        &Value::Array(parsed.clone()).to_string(),
        MISMATCH,
    );
    parsed.push(removed.clone());
    parsed.push(removed);
    rejects(assertion, 0, &Value::Array(parsed).to_string(), MISMATCH);
    rejects(assertion, 0, "[]", MISMATCH);

    // Wrong type: integer release.
    let mut typed: Vec<Value> = serde_json::from_str(&actual).unwrap();
    typed[0]["package_release"] = json!(1);
    rejects(assertion, 0, &Value::Array(typed).to_string(), MISMATCH);

    // Malformed output and failed query.
    rejects(assertion, 0, "dep-liba 1.0.0", NOT_JSON);
    rejects(assertion, 0, "", NOT_JSON);
    rejects(assertion, 1, &actual, "expected exit code 0, got 1");
}

#[test]
fn group_j_t110_requires_exact_untouched_dependency_rows() {
    let manifest =
        load_manifest(&remi_manifest_path("phase3-group-j.toml")).expect("load Group J manifest");
    let test = find_test(&manifest, "T110");
    assert!(
        test.step.iter().all(|step| step
            .conary
            .as_deref()
            .is_none_or(|c| !c.starts_with("list "))),
        "T110 must not use the substring list proof"
    );
    let rows = [
        row("dep-app", "1.0.0"),
        row("dep-base", "1.0.0"),
        row("dep-liba", "1.0.0"),
        row("dep-libb", "1.0.0"),
    ];
    let assertion = row_assertion(&test, T110_QUERY, &Value::Array(rows.to_vec()));
    exercise_controls(
        &assertion,
        T110_QUERY,
        &["dep-app", "dep-base", "dep-liba", "dep-libb"],
        &rows,
    );
}

#[test]
fn group_j_t113_requires_exact_liba_row() {
    let manifest =
        load_manifest(&remi_manifest_path("phase3-group-j.toml")).expect("load Group J manifest");
    let test = find_test(&manifest, "T113");
    assert!(
        test.step.iter().all(|step| step
            .conary
            .as_deref()
            .is_none_or(|c| !c.starts_with("list "))),
        "T113 must not use the substring list proof"
    );
    let rows = [row("dep-liba", "1.0.0")];
    let assertion = row_assertion(&test, T113_QUERY, &Value::Array(rows.to_vec()));
    exercise_controls(&assertion, T113_QUERY, &["dep-liba"], &rows);
}
