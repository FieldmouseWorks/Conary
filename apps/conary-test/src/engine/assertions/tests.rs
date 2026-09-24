// apps/conary-test/src/engine/assertions/tests.rs

#![cfg(test)]

use super::*;

fn base_assertion() -> Assertion {
    Assertion::default()
}

fn json_assertion(pointer: &str, equals: toml::Value) -> Assertion {
    Assertion {
        stdout_json: Some(vec![JsonAssertion {
            pointer: pointer.to_string(),
            equals,
        }]),
        ..Assertion::default()
    }
}

/// Parse a single-key TOML snippet (`v = ...`) into its value.
fn toml_value(source: &str) -> toml::Value {
    let table: toml::Table = toml::from_str(source).unwrap();
    table["v"].clone()
}

#[test]
fn test_stdout_contains_all_pass() {
    let mut a = base_assertion();
    a.stdout_contains_all = Some(vec!["foo".into(), "bar".into()]);
    assert!(evaluate_assertion(&a, 0, "foo bar baz", "").is_ok());
}

#[test]
fn test_stdout_contains_all_fail() {
    let mut a = base_assertion();
    a.stdout_contains_all = Some(vec!["foo".into(), "missing".into()]);
    assert!(evaluate_assertion(&a, 0, "foo bar", "").is_err());
}

#[test]
fn test_stdout_contains_any_pass() {
    let mut a = base_assertion();
    a.stdout_contains_any = Some(vec!["nope".into(), "bar".into()]);
    assert!(evaluate_assertion(&a, 0, "foo bar", "").is_ok());
}

#[test]
fn test_stdout_contains_any_fail() {
    let mut a = base_assertion();
    a.stdout_contains_any = Some(vec!["nope".into(), "missing".into()]);
    assert!(evaluate_assertion(&a, 0, "foo bar", "").is_err());
}

#[test]
fn test_stdout_contains_if_success_skipped_on_failure() {
    let mut a = base_assertion();
    a.stdout_contains_if_success = Some("DRY RUN".into());
    // exit_code != 0, so the assertion is skipped
    assert!(evaluate_assertion(&a, 1, "no match", "").is_ok());
}

#[test]
fn test_stdout_contains_if_success_checked_on_zero() {
    let mut a = base_assertion();
    a.stdout_contains_if_success = Some("DRY RUN".into());
    assert!(evaluate_assertion(&a, 0, "no match", "").is_err());
    assert!(evaluate_assertion(&a, 0, "DRY RUN complete", "").is_ok());
}

#[test]
fn test_stdout_contains_any_if_success_skipped_on_failure() {
    let mut a = base_assertion();
    a.stdout_contains_any_if_success = Some(vec!["composefs".into(), "EROFS".into()]);
    assert!(evaluate_assertion(&a, 1, "no match", "").is_ok());
}

#[test]
fn test_stdout_contains_any_if_success_checked_on_zero() {
    let mut a = base_assertion();
    a.stdout_contains_any_if_success = Some(vec!["composefs".into(), "EROFS".into()]);
    assert!(evaluate_assertion(&a, 0, "using EROFS", "").is_ok());
    assert!(evaluate_assertion(&a, 0, "no match", "").is_err());
}

#[test]
fn test_stderr_not_contains_rejects_forbidden_text() {
    let mut a = base_assertion();
    a.stderr_not_contains = Some("panic".into());

    assert!(evaluate_assertion(&a, 0, "", "warning only").is_ok());
    assert!(evaluate_assertion(&a, 0, "", "thread aborted").is_ok());
    assert!(evaluate_assertion(&a, 0, "", "panic: fixture failed").is_err());
}

#[test]
fn stdout_json_matches_nested_document() {
    let assertion = json_assertion(
        "/data",
        toml_value(
            r#"
            v = { removable = [{ name = "dep-app", version = "1.0.0", architecture = "x86_64" }], skipped = [] }
            "#,
        ),
    );
    let stdout = r#"{"data":{"removable":[{"name":"dep-app","version":"1.0.0","architecture":"x86_64"}],"skipped":[]}}"#;

    assert!(evaluate_assertion(&assertion, 0, stdout, "").is_ok());
}

#[test]
fn stdout_json_rejects_non_json_stdout() {
    let assertion = json_assertion("/status", toml_value(r#"v = "planned""#));

    assert!(evaluate_assertion(&assertion, 0, "not json", "").is_err());
}

#[test]
fn stdout_json_reports_missing_pointer() {
    let assertion = json_assertion("/data/missing", toml_value(r#"v = true"#));

    let error = evaluate_assertion(&assertion, 0, r#"{"data":{}}"#, "").unwrap_err();
    assert!(error.to_string().contains("/data/missing"));
}

#[test]
fn stdout_json_reports_value_mismatch() {
    let assertion = json_assertion("/status", toml_value(r#"v = "planned""#));

    let error = evaluate_assertion(&assertion, 0, r#"{"status":"running"}"#, "").unwrap_err();
    assert!(error.to_string().contains("/status"));
}

#[test]
fn stdout_json_array_order_matters() {
    let assertion = json_assertion("/items", toml_value("v = [1, 2]"));

    assert!(evaluate_assertion(&assertion, 0, r#"{"items":[1,2]}"#, "").is_ok());
    assert!(evaluate_assertion(&assertion, 0, r#"{"items":[2,1]}"#, "").is_err());
}

#[test]
fn stdout_json_numbers_compare_by_typed_value() {
    let integer = json_assertion("/value", toml_value("v = 1"));
    assert!(evaluate_assertion(&integer, 0, r#"{"value":1}"#, "").is_ok());

    let float = json_assertion("/value", toml_value("v = 1.5"));
    assert!(evaluate_assertion(&float, 0, r#"{"value":1.5}"#, "").is_ok());

    let string = json_assertion("/value", toml_value("v = 1"));
    assert!(evaluate_assertion(&string, 0, r#"{"value":"1"}"#, "").is_err());

    let integer_vs_float = json_assertion("/value", toml_value("v = 1"));
    assert!(evaluate_assertion(&integer_vs_float, 0, r#"{"value":1.0}"#, "").is_err());

    let float_vs_integer = json_assertion("/value", toml_value("v = 1.0"));
    assert!(evaluate_assertion(&float_vs_integer, 0, r#"{"value":1}"#, "").is_err());
}

#[test]
fn stdout_json_empty_pointer_compares_whole_document() {
    let assertion = json_assertion("", toml_value(r#"v = { status = "planned" }"#));

    assert!(evaluate_assertion(&assertion, 0, r#"{"status":"planned"}"#, "").is_ok());
    assert!(evaluate_assertion(&assertion, 0, r#"{"status":"other"}"#, "").is_err());
}
