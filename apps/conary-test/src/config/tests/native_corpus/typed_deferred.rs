// apps/conary-test/src/config/tests/native_corpus/typed_deferred.rs
#![cfg(test)]

use super::super::{load_manifest, remi_manifest_path};
use crate::config::manifest::{Assertion, JsonExpectation, TestDef, TestStep};
use crate::engine::{
    assertions::evaluate_assertion,
    variables::{expand_assertion, expand_variables},
};
use serde_json::{Value, json};

const PROJECTION: &str = "SELECT json_extract(metadata, '$.schema') AS schema, json_type(metadata, '$.deferred_follow_up') AS follow_up_type, json_array_length(metadata, '$.deferred_follow_up') AS follow_up_count, json_extract(metadata, '$.deferred_follow_up[0].kind') AS kind, json_extract(metadata, '$.deferred_follow_up[0].status') AS status, json_extract(metadata, '$.deferred_follow_up[0].message') AS message";
const LANES: &[&str] = &["fedora44", "ubuntu-26.04", "arch"];

#[test]
fn native_parity_deferred_metadata_requires_exact_typed_rows_for_all_lanes() {
    let manifest = load_manifest(&remi_manifest_path("phase4-native-pm-parity.toml"))
        .expect("load native parity manifest");
    let test = manifest
        .test
        .iter()
        .find(|test| test.id == "TNPM09")
        .expect("TNPM09 must own deferred metadata");
    assert_eq!(test.step.len(), 4);
    assert_preserved_observations(test);

    let metadata = &test.step[1];
    let command = metadata
        .run
        .as_deref()
        .expect("metadata step must run sqlite3");
    let query = sqlite_query(metadata);
    assert_eq!(
        query,
        format!(
            "{PROJECTION} FROM changesets WHERE description = 'Install phase4-runtime-fixture-${{native_fixture_version}}' ORDER BY id DESC LIMIT 1"
        )
    );
    assert_eq!(command, format!("sqlite3 -json ${{DB_PATH}} \"{query}\""));
    assert_eq!(query.matches("SELECT").count(), 1);
    assert!(!command.contains("COALESCE(metadata"));

    let assertion = typed_root_assertion(metadata);
    assert_eq!(
        assertion.stdout_json.as_ref().unwrap()[0].expected,
        JsonExpectation::Equals(expected_row())
    );

    for &distro in LANES {
        let overrides = manifest
            .distro_overrides
            .get(distro)
            .unwrap_or_else(|| panic!("missing {distro} manifest overrides"));
        let version = overrides
            .get("native_fixture_version")
            .unwrap_or_else(|| panic!("missing {distro} native_fixture_version"));
        let expanded = expand_variables(command, overrides);
        assert_eq!(
            expanded,
            format!(
                "sqlite3 -json ${{DB_PATH}} \"{PROJECTION} FROM changesets WHERE description = 'Install phase4-runtime-fixture-{version}' ORDER BY id DESC LIMIT 1\""
            ),
            "{distro} must expand the exact changeset description predicate"
        );

        let check = expand_assertion(assertion, overrides);
        let valid = expected_row();
        assert!(
            evaluate_assertion(&check, 0, &valid.to_string(), "").is_ok(),
            "{distro} must accept the exact typed deferred metadata row"
        );
        demonstrate_resolved_false_positive(&check);
        reject_bad_results(&check, &valid);
    }
}

fn assert_preserved_observations(test: &TestDef) {
    assert!(test.step[0].run.as_deref().is_some_and(|run| {
        run.contains("CONARY_TEST_FAIL_GENERATION_REBUILD=slice-d-forced")
            && run.contains(" install ")
    }));
    assert_eq!(
        test.step[0]
            .assert
            .as_ref()
            .and_then(|assertion| assertion.stdout_contains.as_deref()),
        Some("pending")
    );
    assert!(test.step[2].run.as_deref().is_some_and(|run| {
        run.contains("FROM generation_publications") && run.contains("ORDER BY id DESC LIMIT 1")
    }));
    assert_eq!(test.step[3].conary.as_deref(), Some("system history"));
    let expected_history = [
        "Install phase4-runtime-fixture-${native_fixture_version}".to_owned(),
        "Deferred work (1):".to_owned(),
        "  Kind: generation_publication".to_owned(),
    ];
    assert_eq!(
        test.step[3]
            .assert
            .as_ref()
            .and_then(|assertion| assertion.stdout_contains_all.as_deref()),
        Some(expected_history.as_slice())
    );
}

fn sqlite_query(step: &TestStep) -> &str {
    let command = step
        .run
        .as_deref()
        .expect("deferred metadata step must run sqlite3");
    command
        .strip_prefix("sqlite3 -json ${DB_PATH} \"")
        .and_then(|query| query.strip_suffix('"'))
        .expect("deferred metadata query must use sqlite3 -json")
}

fn typed_root_assertion(step: &TestStep) -> &Assertion {
    let assertion = step
        .assert
        .as_ref()
        .expect("deferred metadata query needs an assertion");
    assert_eq!(assertion.exit_code, Some(0));
    assert!(assertion.exit_code_not.is_none());
    assert!(assertion.stdout_contains.is_none());
    assert!(assertion.stdout_contains_all.is_none());
    assert!(assertion.stdout_contains_any.is_none());
    assert!(assertion.stdout_contains_if_success.is_none());
    assert!(assertion.stdout_contains_any_if_success.is_none());
    assert!(assertion.stdout_not_contains.is_none());
    assert!(assertion.stderr_contains.is_none());
    assert!(assertion.stderr_not_contains.is_none());
    let checks = assertion
        .stdout_json
        .as_ref()
        .expect("deferred metadata must use exact JSON assertion");
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].pointer, "");
    assert!(matches!(checks[0].expected, JsonExpectation::Equals(_)));
    assertion
}

fn expected_row() -> Value {
    json!([{
        "schema": "conary.changeset.metadata.v7",
        "follow_up_type": "array",
        "follow_up_count": 1,
        "kind": "generation_publication",
        "status": "pending",
        "message": "generation publication is pending",
    }])
}

fn demonstrate_resolved_false_positive(assertion: &Assertion) {
    let old_check = Assertion {
        stdout_contains_all: Some(vec![
            "deferred_follow_up".into(),
            "generation_publication".into(),
            "generation publication is pending".into(),
        ]),
        ..Assertion::default()
    };
    let old_metadata = json!({
        "schema": "conary.changeset.metadata.v7",
        "deferred_follow_up": [{
            "kind": "generation_publication",
            "status": "resolved",
            "message": "generation publication is pending",
        }],
    });
    assert!(evaluate_assertion(&old_check, 0, &old_metadata.to_string(), "").is_ok());

    let mut resolved = expected_row();
    resolved[0]["status"] = json!("resolved");
    rejects(
        assertion,
        &resolved.to_string(),
        "resolved follow-up status",
    );
}

fn reject_bad_results(assertion: &Assertion, expected: &Value) {
    for (field, value) in [
        ("schema", json!("conary.changeset.metadata.v6")),
        ("follow_up_type", json!("object")),
        ("follow_up_count", json!(0)),
        ("kind", json!("other")),
        ("status", json!("failed")),
        ("message", json!("generation publication failed")),
    ] {
        let mut changed = expected.clone();
        changed[0][field] = value;
        rejects(assertion, &changed.to_string(), field);
    }
    for (field, value) in [
        ("schema", Value::Null),
        ("schema", json!(7)),
        ("follow_up_type", Value::Null),
        ("follow_up_type", json!(true)),
        ("follow_up_count", Value::Null),
        ("follow_up_count", json!("1")),
        ("follow_up_count", json!(1.0)),
        ("kind", Value::Null),
        ("kind", json!(1)),
        ("status", Value::Null),
        ("status", json!(false)),
        ("message", Value::Null),
        ("message", json!([])),
    ] {
        let mut changed = expected.clone();
        changed[0][field] = value;
        rejects(
            assertion,
            &changed.to_string(),
            &format!("invalid {field} value"),
        );
    }

    rejects(assertion, "[]", "missing row");
    rejects(
        assertion,
        &json!([expected[0].clone(), expected[0].clone()]).to_string(),
        "extra row",
    );
    let mut missing_key = expected.clone();
    missing_key[0].as_object_mut().unwrap().remove("kind");
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
        "must reject a failed sqlite3 command with valid JSON"
    );
}

fn rejects(assertion: &Assertion, stdout: &str, defect: &str) {
    assert!(
        evaluate_assertion(assertion, 0, stdout, "").is_err(),
        "must reject {defect}: {stdout}"
    );
}
