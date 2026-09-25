// apps/conary-test/src/engine/assertions/tests.rs

#![cfg(test)]

use super::*;

fn base_assertion() -> Assertion {
    Assertion::default()
}

fn json_assertion(pointer: &str, equals: serde_json::Value) -> Assertion {
    Assertion {
        stdout_json: Some(vec![JsonAssertion {
            pointer: pointer.to_string(),
            expected: JsonExpectation::Equals(equals),
        }]),
        ..Assertion::default()
    }
}

fn json_null_assertion(pointer: &str) -> Assertion {
    Assertion {
        stdout_json: Some(vec![JsonAssertion {
            pointer: pointer.to_string(),
            expected: JsonExpectation::Null,
        }]),
        ..Assertion::default()
    }
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
        serde_json::json!({
            "removable": [{ "name": "dep-app", "version": "1.0.0", "architecture": "x86_64" }],
            "skipped": [],
        }),
    );
    let stdout = r#"{"data":{"removable":[{"name":"dep-app","version":"1.0.0","architecture":"x86_64"}],"skipped":[]}}"#;

    assert!(evaluate_assertion(&assertion, 0, stdout, "").is_ok());
}

#[test]
fn stdout_json_rejects_non_json_stdout() {
    let assertion = json_assertion("/status", serde_json::json!("planned"));

    assert!(evaluate_assertion(&assertion, 0, "not json", "").is_err());
}

#[test]
fn stdout_json_reports_missing_pointer() {
    let assertion = json_assertion("/data/missing", serde_json::json!(true));

    let error = evaluate_assertion(&assertion, 0, r#"{"data":{}}"#, "").unwrap_err();
    assert!(error.to_string().contains("/data/missing"));
}

#[test]
fn stdout_json_reports_value_mismatch() {
    let assertion = json_assertion("/status", serde_json::json!("planned"));

    let error = evaluate_assertion(&assertion, 0, r#"{"status":"running"}"#, "").unwrap_err();
    assert!(error.to_string().contains("/status"));
}

#[test]
fn stdout_json_array_order_matters() {
    let assertion = json_assertion("/items", serde_json::json!([1, 2]));

    assert!(evaluate_assertion(&assertion, 0, r#"{"items":[1,2]}"#, "").is_ok());
    assert!(evaluate_assertion(&assertion, 0, r#"{"items":[2,1]}"#, "").is_err());
}

#[test]
fn stdout_json_numbers_compare_by_typed_value() {
    let integer = json_assertion("/value", serde_json::json!(1));
    assert!(evaluate_assertion(&integer, 0, r#"{"value":1}"#, "").is_ok());

    let float = json_assertion("/value", serde_json::json!(1.5));
    assert!(evaluate_assertion(&float, 0, r#"{"value":1.5}"#, "").is_ok());

    let string = json_assertion("/value", serde_json::json!(1));
    assert!(evaluate_assertion(&string, 0, r#"{"value":"1"}"#, "").is_err());

    let integer_vs_float = json_assertion("/value", serde_json::json!(1));
    assert!(evaluate_assertion(&integer_vs_float, 0, r#"{"value":1.0}"#, "").is_err());

    let float_vs_integer = json_assertion("/value", serde_json::json!(1.0));
    assert!(evaluate_assertion(&float_vs_integer, 0, r#"{"value":1}"#, "").is_err());
}

#[test]
fn stdout_json_equals_json_matches_unsigned_integer_above_i64_max() {
    // This is the value the `equals_json` load path produces; the exact token
    // must stay a `u64` through comparison.
    let expected: serde_json::Value = serde_json::from_str("18446744073709551615").unwrap();
    assert_eq!(expected.as_u64(), Some(u64::MAX));
    let assertion = json_assertion("/id", expected);

    // Positive control through the same fixture.
    assert!(evaluate_assertion(&assertion, 0, r#"{"id":18446744073709551615}"#, "").is_ok());

    let error =
        evaluate_assertion(&assertion, 0, r#"{"id":18446744073709551614}"#, "").unwrap_err();
    assert!(error.to_string().contains("/id"), "{error}");
}

#[test]
fn stdout_json_empty_pointer_compares_whole_document() {
    let assertion = json_assertion("", serde_json::json!({ "status": "planned" }));

    assert!(evaluate_assertion(&assertion, 0, r#"{"status":"planned"}"#, "").is_ok());
    assert!(evaluate_assertion(&assertion, 0, r#"{"status":"other"}"#, "").is_err());
}

#[test]
fn stdout_json_null_matches_json_null() {
    let assertion = json_null_assertion("/x");

    assert!(evaluate_assertion(&assertion, 0, r#"{"x":null}"#, "").is_ok());
}

#[test]
fn stdout_json_null_rejects_non_null_values() {
    let assertion = json_null_assertion("/x");

    for stdout in [r#"{"x":0}"#, r#"{"x":""}"#, r#"{"x":false}"#] {
        assert!(
            evaluate_assertion(&assertion, 0, stdout, "").is_err(),
            "null assertion unexpectedly accepted {stdout}"
        );
    }
}

#[test]
fn stdout_json_null_reports_missing_pointer() {
    let assertion = json_null_assertion("/x");

    assert!(evaluate_assertion(&assertion, 0, r#"{}"#, "").is_err());
}

#[test]
fn stdout_json_refuses_out_of_range_actual_integer() {
    let expected: serde_json::Value = serde_json::from_str("18446744073709551615").unwrap();
    assert_eq!(expected.as_u64(), Some(u64::MAX));
    let assertion = json_assertion("/id", expected);

    // Positive control through the same fixture.
    assert!(evaluate_assertion(&assertion, 0, r#"{"id":18446744073709551615}"#, "").is_ok());

    // Negative: `u64::MAX + 1` is an integer token outside the exact range.
    // `serde_json` rounds it to the same `f64`, so the assertion must refuse
    // it rather than match through rounding, and name the source token.
    let error =
        evaluate_assertion(&assertion, 0, r#"{"id":18446744073709551617}"#, "").unwrap_err();
    let message = error.to_string();
    assert!(message.contains("/id"), "{message}");
    assert!(message.contains("18446744073709551617"), "{message}");
    assert!(message.contains("exactly comparable"), "{message}");
}

#[test]
fn stdout_json_refuses_out_of_range_actual_integer_even_against_float() {
    // The float expectation equals the `f64` that `18446744073709551617`
    // rounds to, so only token awareness keeps the comparison sound. Both the
    // expectation and the positive control come from `serde_json` so the test
    // does not depend on the float parser's rounding mode.
    let expected: serde_json::Value = serde_json::from_str("1.8446744073709552e19").unwrap();
    let assertion = json_assertion("/id", expected);

    // Positive control: a real float token with that value matches.
    assert!(evaluate_assertion(&assertion, 0, r#"{"id":1.8446744073709552e19}"#, "").is_ok());

    // Negative: an out-of-range integer token must not match as a float.
    let error =
        evaluate_assertion(&assertion, 0, r#"{"id":18446744073709551617}"#, "").unwrap_err();
    assert!(error.to_string().contains("exactly comparable"), "{error}");
}

#[test]
fn stdout_json_refuses_nested_out_of_range_actual_integer() {
    let assertion = json_assertion("/data", serde_json::json!({ "id": 1 }));

    // Positive control through the same fixture.
    assert!(evaluate_assertion(&assertion, 0, r#"{"data":{"id":1}}"#, "").is_ok());

    // Negative: the error names the nested pointer, not just the check's.
    let error = evaluate_assertion(&assertion, 0, r#"{"data":{"id":18446744073709551617}}"#, "")
        .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("/data/id"), "{message}");
    assert!(message.contains("18446744073709551617"), "{message}");
}

#[test]
fn stdout_json_accepts_big_integer_digits_in_string() {
    let assertion = json_assertion("/id", serde_json::json!("18446744073709551617"));

    assert!(evaluate_assertion(&assertion, 0, r#"{"id":"18446744073709551617"}"#, "").is_ok());
}

#[test]
fn find_inexact_json_integers_reports_nested_pointers() {
    let found = find_inexact_json_integers(
        r#"{"a":[1,18446744073709551616],"b":{"c":-9223372036854775809}}"#,
    )
    .unwrap();

    let pointers: Vec<&str> = found
        .iter()
        .map(|integer| integer.pointer.as_str())
        .collect();
    assert_eq!(pointers, vec!["/a/1", "/b/c"]);
}

#[test]
fn find_inexact_json_integers_ignores_exact_and_float_tokens() {
    let found = find_inexact_json_integers(
        r#"{"max":18446744073709551615,"min":-9223372036854775808,"float":1.5e3,"text":"18446744073709551616"}"#,
    )
    .unwrap();

    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn find_inexact_json_integers_escapes_pointer_tokens() {
    let found = find_inexact_json_integers(r#"{"a/b":{"c~d":18446744073709551616}}"#).unwrap();

    assert_eq!(found.len(), 1);
    assert_eq!(found[0].pointer, "/a~1b/c~0d");
}

#[test]
fn find_inexact_json_integers_decodes_string_escapes_in_keys() {
    let found = find_inexact_json_integers(r#"{"\u0061":18446744073709551616}"#).unwrap();

    assert_eq!(found.len(), 1);
    assert_eq!(found[0].pointer, "/a");
}
