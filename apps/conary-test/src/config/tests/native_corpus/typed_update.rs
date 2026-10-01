// apps/conary-test/src/config/tests/native_corpus/typed_update.rs
#![cfg(test)]

use super::super::{load_manifest, remi_manifest_path};
use crate::config::manifest::{Assertion, JsonExpectation, TestStep};
use crate::engine::{assertions::evaluate_assertion, variables::expand_assertion};
use serde_json::{Value, json};
use std::collections::HashMap;

const PACKAGE: &str = "phase4-runtime-fixture";
const LANES: &[&str] = &["fedora44", "ubuntu-26.04", "arch"];

#[test]
fn native_parity_update_requires_exact_typed_rows_for_all_lanes() {
    let manifest = load_manifest(&remi_manifest_path("phase4-native-pm-parity.toml"))
        .expect("load native parity manifest");
    let test = manifest
        .test
        .iter()
        .find(|test| test.id == "TNPM07")
        .expect("TNPM07 must own the local native update");
    assert_eq!(test.step.len(), 7);
    assert_eq!(
        test.step[0].conary.as_deref(),
        Some("unpin phase4-runtime-fixture")
    );
    assert!(
        test.step[1]
            .run
            .as_deref()
            .is_some_and(|run| run.contains("prepare-signed-update-repository.sh"))
    );
    assert!(
        test.step[3]
            .run
            .as_deref()
            .is_some_and(|run| run.contains("update phase4-runtime-fixture"))
    );
    assert!(
        test.step[4]
            .run
            .as_deref()
            .is_some_and(|run| run.contains("assert-selected-generation.py"))
    );
    assert_eq!(
        test.step[6].conary.as_deref(),
        Some("list phase4-runtime-fixture --info")
    );

    let repository = &test.step[2];
    let installed = &test.step[5];
    assert_repository_query(repository);
    assert_trove_query(installed);

    for &distro in LANES {
        let vars = manifest
            .distro_overrides
            .get(distro)
            .unwrap_or_else(|| panic!("missing {distro} manifest overrides"));
        let repository_assertion = expand_assertion(repository.assert.as_ref().unwrap(), vars);
        let installed_assertion = expand_assertion(installed.assert.as_ref().unwrap(), vars);
        let expected_repository = expected_repository_row(vars);
        let expected_installed = expected_installed_row(vars);
        let repository_json = expected_repository.to_string();
        let installed_json = expected_installed.to_string();

        assert!(evaluate_assertion(&repository_assertion, 0, &repository_json, "").is_ok());
        assert!(evaluate_assertion(&installed_assertion, 0, &installed_json, "").is_ok());
        demonstrate_substring_false_positive(
            &repository_assertion,
            &expected_repository,
            format!(
                "binary|{}|{PACKAGE}|{}|{}|{}|active",
                variable(vars, "native_profile"),
                variable(vars, "native_update_version"),
                variable(vars, "native_scheme"),
                variable(vars, "native_arch"),
            ),
            format!(
                "binary|{}|{PACKAGE}|{}|{}|{}|retired",
                variable(vars, "native_profile"),
                variable(vars, "native_update_version"),
                variable(vars, "native_scheme"),
                variable(vars, "native_arch"),
            ),
            with_string_field(&expected_repository, "key_status", "retired"),
        );
        demonstrate_substring_false_positive(
            &installed_assertion,
            &expected_installed,
            format!(
                "{PACKAGE}|{}|{}|{}|{}",
                variable(vars, "native_update_version"),
                variable(vars, "native_arch"),
                variable(vars, "native_scheme"),
                variable(vars, "native_profile"),
            ),
            format!(
                "{PACKAGE}|99.99.99-wrong|{}|{}|{}",
                variable(vars, "native_arch"),
                variable(vars, "native_scheme"),
                variable(vars, "native_profile"),
            ),
            with_string_field(&expected_installed, "version", "99.99.99-wrong"),
        );
        reject_bad_results(&repository_assertion, &expected_repository, "key_status");
        reject_bad_results(&installed_assertion, &expected_installed, "source_profile");
    }
}

fn assert_repository_query(step: &TestStep) {
    let sql = sqlite_query(step);
    let (select, from) = sql
        .split_once(" FROM ")
        .expect("repository metadata query must have a FROM clause");
    assert_eq!(
        select
            .strip_prefix("SELECT ")
            .unwrap()
            .split(", ")
            .collect::<Vec<_>>(),
        [
            "r.default_strategy AS default_strategy",
            "r.source_profile AS source_profile",
            "rp.name AS name",
            "rp.version AS version",
            "rp.version_scheme AS version_scheme",
            "rp.architecture AS architecture",
            "k.status AS key_status",
        ]
    );
    assert_eq!(
        from,
        "repositories r JOIN repository_packages rp ON rp.repository_id = r.id JOIN repository_package_keys k ON k.repository_id = r.id WHERE r.name = 'slice-d-local-update'"
    );
    assert_eq!(sql.matches("SELECT").count(), 1);
    assert!(!sql.contains(';'));
    assert_typed_root_assertion(step);
}

fn assert_trove_query(step: &TestStep) {
    let sql = sqlite_query(step);
    let (select, from) = sql
        .split_once(" FROM ")
        .expect("installed package query must have a FROM clause");
    assert_eq!(
        select,
        "SELECT name, version, COALESCE(architecture, '') AS architecture, COALESCE(version_scheme, '') AS version_scheme, COALESCE(source_profile, '') AS source_profile"
    );
    assert_eq!(from, "troves WHERE name = 'phase4-runtime-fixture'");
    assert_eq!(sql.matches("SELECT").count(), 1);
    assert!(!sql.contains(';'));
    assert_typed_root_assertion(step);
}

fn sqlite_query(step: &TestStep) -> &str {
    let command = step
        .run
        .as_deref()
        .expect("typed row step must run sqlite3");
    assert!(command.starts_with("sqlite3 -json ${DB_PATH} \""));
    command
        .strip_prefix("sqlite3 -json ${DB_PATH} \"")
        .and_then(|sql| sql.strip_suffix('"'))
        .expect("typed row query must be one quoted command")
}

fn assert_typed_root_assertion(step: &TestStep) {
    let assertion = step
        .assert
        .as_ref()
        .expect("typed row step needs an assertion");
    assert_eq!(assertion.exit_code, Some(0));
    assert!(
        assertion.stdout_contains.is_none()
            && assertion.stdout_contains_all.is_none()
            && assertion.stdout_contains_any.is_none()
    );
    let checks = assertion
        .stdout_json
        .as_ref()
        .expect("exact JSON assertion");
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].pointer, "");
    assert!(matches!(checks[0].expected, JsonExpectation::Equals(_)));
}

fn expected_repository_row(vars: &HashMap<String, String>) -> Value {
    json!([{
        "default_strategy": "binary",
        "source_profile": variable(vars, "native_profile"),
        "name": PACKAGE,
        "version": variable(vars, "native_update_version"),
        "version_scheme": variable(vars, "native_scheme"),
        "architecture": variable(vars, "native_arch"),
        "key_status": "active",
    }])
}

fn expected_installed_row(vars: &HashMap<String, String>) -> Value {
    json!([{
        "name": PACKAGE,
        "version": variable(vars, "native_update_version"),
        "architecture": variable(vars, "native_arch"),
        "version_scheme": variable(vars, "native_scheme"),
        "source_profile": variable(vars, "native_profile"),
    }])
}

fn variable<'a>(vars: &'a HashMap<String, String>, name: &str) -> &'a str {
    vars.get(name)
        .unwrap_or_else(|| panic!("missing manifest variable {name}"))
}

fn demonstrate_substring_false_positive(
    assertion: &Assertion,
    expected: &Value,
    expected_text: String,
    extra_text: String,
    extra_row: Value,
) {
    let old_assertion = Assertion {
        stdout_contains: Some(expected_text.clone()),
        ..Assertion::default()
    };
    let old_output = format!("{expected_text}\n{extra_text}");
    assert!(evaluate_assertion(&old_assertion, 0, &old_output, "").is_ok());
    reject_extra_row(assertion, expected, extra_row);
}

fn with_string_field(expected: &Value, field: &str, value: &str) -> Value {
    let mut row = expected[0].clone();
    row[field] = json!(value);
    row
}

fn reject_extra_row(assertion: &Assertion, expected: &Value, extra: Value) {
    let result = json!([expected[0].clone(), extra]);
    rejects(assertion, &result.to_string(), "extra row");
}

fn reject_bad_results(assertion: &Assertion, expected: &Value, nullable_field: &str) {
    let row = expected[0].as_object().unwrap();
    for field in row.keys() {
        let mut changed = expected.clone();
        changed[0][field] = json!("wrong-value");
        rejects(assertion, &changed.to_string(), field);
    }

    let mut wrong_type = expected.clone();
    wrong_type[0]["version"] = json!(1);
    rejects(assertion, &wrong_type.to_string(), "wrong field type");

    let mut null_value = expected.clone();
    null_value[0][nullable_field] = Value::Null;
    rejects(assertion, &null_value.to_string(), "null field");

    rejects(assertion, "[]", "missing row");
    rejects(
        assertion,
        &json!([expected[0].clone(), expected[0].clone()]).to_string(),
        "extra row",
    );
    let mut missing_key = expected.clone();
    assert!(
        missing_key[0]
            .as_object_mut()
            .unwrap()
            .remove(nullable_field)
            .is_some()
    );
    rejects(assertion, &missing_key.to_string(), "missing key");

    let mut extra_key = expected.clone();
    extra_key[0]["unexpected"] = json!(true);
    rejects(assertion, &extra_key.to_string(), "extra key");

    let valid = expected.to_string();
    rejects(assertion, "not JSON", "malformed JSON");
    rejects(
        assertion,
        &format!("{valid}{valid}"),
        "concatenated JSON documents",
    );
    assert!(
        evaluate_assertion(assertion, 1, &valid, "").is_err(),
        "must reject a nonzero command exit"
    );
}

fn rejects(assertion: &Assertion, stdout: &str, defect: &str) {
    assert!(
        evaluate_assertion(assertion, 0, stdout, "").is_err(),
        "must reject {defect}: {stdout}"
    );
}
