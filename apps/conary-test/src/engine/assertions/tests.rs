// apps/conary-test/src/engine/assertions/tests.rs

#![cfg(test)]

use super::*;
use std::collections::HashMap;

fn base_assertion() -> Assertion {
    Assertion::default()
}

fn json_assertion(pointer: &str, equals: serde_json::Value) -> Assertion {
    Assertion {
        stdout_json: Some(vec![JsonAssertion {
            pointer: pointer.to_string(),
            expected: JsonExpectation::Equals(equals),
            numbers: HashMap::new(),
        }]),
        ..Assertion::default()
    }
}

fn json_null_assertion(pointer: &str) -> Assertion {
    Assertion {
        stdout_json: Some(vec![JsonAssertion {
            pointer: pointer.to_string(),
            expected: JsonExpectation::Null,
            numbers: HashMap::new(),
        }]),
        ..Assertion::default()
    }
}

/// Build the assertion the `equals_json` load path produces for `text`: the
/// parsed value plus the walker's exact source tokens.
fn json_text_assertion(pointer: &str, text: &str) -> Assertion {
    let value: serde_json::Value = serde_json::from_str(text).unwrap();
    let numbers = find_json_number_tokens(text)
        .unwrap()
        .into_iter()
        .map(|number| (number.pointer, number.token))
        .collect();
    Assertion {
        stdout_json: Some(vec![JsonAssertion {
            pointer: pointer.to_string(),
            expected: JsonExpectation::Equals(value),
            numbers,
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
fn stdout_json_reports_out_of_range_number_instead_of_invalid_json() {
    let assertion = json_assertion("/n", serde_json::json!(0));
    let digits = "9".repeat(400);
    let stdout = format!(r#"{{"n":{digits}}}"#);

    let error = evaluate_assertion(&assertion, 0, &stdout, "").unwrap_err();
    let message = error.to_string();
    assert!(message.contains("/n"), "{message}");
    assert!(message.contains("beyond the supported range"), "{message}");
    assert!(!message.contains("not valid JSON"), "{message}");
}

#[test]
fn stdout_json_reports_syntax_error_for_malformed_stdout() {
    // Positive control for the range path: the same fixture reports the range
    // message when the document is valid apart from the number's magnitude.
    let assertion = json_assertion("/n", serde_json::json!(0));
    let digits = "9".repeat(400);
    let error = evaluate_assertion(&assertion, 0, &format!(r#"{{"n":{digits}}}"#), "").unwrap_err();
    assert!(
        error.to_string().contains("beyond the supported range"),
        "{error}"
    );

    // Negative: a genuinely malformed document keeps the generic syntax
    // message, because the walker cannot scan it.
    let error = evaluate_assertion(&assertion, 0, r#"{"n": }"#, "").unwrap_err();
    let message = error.to_string();
    assert!(message.contains("not valid JSON"), "{message}");
    assert!(!message.contains("beyond the supported range"), "{message}");
}

#[test]
fn stdout_json_reports_control_character_as_syntax_error() {
    // A malformed string must not be called valid just because the document
    // also contains an out-of-range number; only the range cause is excused.
    let assertion = json_assertion("/n", serde_json::json!(0));
    let digits = "9".repeat(400);
    let stdout = format!("{{\"s\":\"raw\ncontrol\",\"n\":{digits}}}");

    let error = evaluate_assertion(&assertion, 0, &stdout, "").unwrap_err();
    let message = error.to_string();
    assert!(message.contains("not valid JSON"), "{message}");
    assert!(!message.contains("beyond the supported range"), "{message}");
}

#[test]
fn stdout_json_reports_malformed_number_as_syntax_error() {
    // The document also contains an out-of-range number, but the malformed
    // leading-zero number means a magnitude is not the only defect.
    let assertion = json_assertion("/n", serde_json::json!(0));
    let digits = "9".repeat(400);
    let stdout = format!(r#"{{"a":01,"n":{digits}}}"#);

    let error = evaluate_assertion(&assertion, 0, &stdout, "").unwrap_err();
    let message = error.to_string();
    assert!(message.contains("not valid JSON"), "{message}");
    assert!(!message.contains("beyond the supported range"), "{message}");
}

#[test]
fn number_exceeds_f64_classifies_boundaries() {
    // Finite boundaries must not be flagged.
    for token in [
        "0",
        "1",
        "-1",
        "18446744073709551616",
        "1e308",
        "1.7976931348623157e308",
        "1e-999",
    ] {
        assert!(!number_exceeds_f64(token), "{token} unexpectedly flagged");
    }

    // An order above 308, top-order infinity, and an exponent too large for
    // `i64` are all beyond finite range.
    let beyond = [
        "1e309".to_string(),
        "1.8e308".to_string(),
        "9".repeat(400),
        "1e99999999999999999999".to_string(),
    ];
    for token in &beyond {
        assert!(number_exceeds_f64(token), "{token} unexpectedly accepted");
    }

    // A malformed token is a syntax concern, not a range one.
    for token in ["01e999", "1.2.3e999", "1e", "+1e999"] {
        assert!(!number_exceeds_f64(token), "{token} unexpectedly flagged");
    }
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

    // A number never matches a string, and an integer never matches a decimal.
    let string = json_assertion("/value", serde_json::json!(1));
    let error = evaluate_assertion(&string, 0, r#"{"value":"1"}"#, "").unwrap_err();
    assert!(error.to_string().contains("did not match"), "{error}");

    let integer_vs_float = json_assertion("/value", serde_json::json!(1));
    let error = evaluate_assertion(&integer_vs_float, 0, r#"{"value":1.0}"#, "").unwrap_err();
    assert!(error.to_string().contains("did not match"), "{error}");

    let float_vs_integer = json_assertion("/value", serde_json::json!(1.0));
    let error = evaluate_assertion(&float_vs_integer, 0, r#"{"value":1}"#, "").unwrap_err();
    assert!(error.to_string().contains("did not match"), "{error}");
}

#[test]
fn stdout_json_equals_json_matches_unsigned_integer_above_i64_max() {
    // The `equals_json` load path keeps this exact token even though
    // `serde_json` stores the value as an `f64`.
    let assertion = json_text_assertion("/id", "18446744073709551615");

    // Positive control through the same fixture.
    assert!(evaluate_assertion(&assertion, 0, r#"{"id":18446744073709551615}"#, "").is_ok());

    let error =
        evaluate_assertion(&assertion, 0, r#"{"id":18446744073709551614}"#, "").unwrap_err();
    assert!(error.to_string().contains("/id"), "{error}");
}

#[test]
fn stdout_json_equals_json_compares_decimals_without_rounding() {
    // `9007199254740992.0` and `9007199254740993.0` round to the same `f64`
    // (2^53), so only exact decimal comparison rejects the second.
    let assertion = json_text_assertion("/value", "9007199254740992.0");

    // Positive control through the same fixture.
    assert!(evaluate_assertion(&assertion, 0, r#"{"value":9007199254740992.0}"#, "").is_ok());

    let error =
        evaluate_assertion(&assertion, 0, r#"{"value":9007199254740993.0}"#, "").unwrap_err();
    let message = error.to_string();
    assert!(message.contains("/value"), "{message}");
    assert!(message.contains("did not match"), "{message}");
}

#[test]
fn stdout_json_equals_json_uses_the_exact_expected_token() {
    // The expectation itself carries more precision than `f64` can hold, so a
    // comparison that reduced it to `f64` would accept the rounded neighbor.
    let assertion = json_text_assertion("/value", "9007199254740993.0");

    // Positive control: the same exact decimal matches.
    assert!(evaluate_assertion(&assertion, 0, r#"{"value":9007199254740993.0}"#, "").is_ok());

    // Negative: the `f64`-representable neighbor rounds to the same binary
    // float but is a different exact decimal value.
    let error =
        evaluate_assertion(&assertion, 0, r#"{"value":9007199254740992.0}"#, "").unwrap_err();
    assert!(error.to_string().contains("did not match"), "{error}");
}

#[test]
fn stdout_json_equals_json_treats_equivalent_decimals_as_equal() {
    for (expected, actual) in [
        ("1.5", "1.50"),
        ("1.50", "15e-1"),
        ("15e-1", "0.15E1"),
        ("0.15E1", "1.5"),
    ] {
        let assertion = json_text_assertion("/value", expected);
        let stdout = format!(r#"{{"value":{actual}}}"#);
        assert!(
            evaluate_assertion(&assertion, 0, &stdout, "").is_ok(),
            "expected {expected} to equal {actual}"
        );
    }
}

#[test]
fn stdout_json_equals_json_refuses_integer_decimal_cross_match() {
    // The canonical decimal values agree, so only the integer/decimal kind
    // policy keeps `1` and `1.0` distinct.
    let integer_expectation = json_text_assertion("/value", "1");
    assert!(evaluate_assertion(&integer_expectation, 0, r#"{"value":1}"#, "").is_ok());
    let error = evaluate_assertion(&integer_expectation, 0, r#"{"value":1.0}"#, "").unwrap_err();
    assert!(error.to_string().contains("did not match"), "{error}");

    let decimal_expectation = json_text_assertion("/value", "1.0");
    assert!(evaluate_assertion(&decimal_expectation, 0, r#"{"value":1.0}"#, "").is_ok());
    let error = evaluate_assertion(&decimal_expectation, 0, r#"{"value":1}"#, "").unwrap_err();
    assert!(error.to_string().contains("did not match"), "{error}");
}

#[test]
fn stdout_json_equals_json_compares_big_integers_exactly() {
    let assertion = json_text_assertion("/id", "18446744073709551617");

    // Positive control: the same big integer matches.
    assert!(evaluate_assertion(&assertion, 0, r#"{"id":18446744073709551617}"#, "").is_ok());

    // Negative: one below is a different exact integer even though both round
    // to the same `f64`.
    let error =
        evaluate_assertion(&assertion, 0, r#"{"id":18446744073709551616}"#, "").unwrap_err();
    let message = error.to_string();
    assert!(message.contains("/id"), "{message}");
    assert!(message.contains("did not match"), "{message}");
}

#[test]
fn stdout_json_equals_json_compares_u64_max_plus_one_exactly() {
    // `u64::MAX + 1` exceeds `u64`, so `serde_json` stores it as a rounded
    // `f64`; the recorded source token keeps the comparison exact. This is the
    // finite upper boundary just below the unsupported range.
    let assertion = json_text_assertion("/id", "18446744073709551616");

    // Positive control: the same big integer matches.
    assert!(evaluate_assertion(&assertion, 0, r#"{"id":18446744073709551616}"#, "").is_ok());

    // Negative: `u64::MAX` rounds to the same `f64` but is a different exact
    // integer.
    let error =
        evaluate_assertion(&assertion, 0, r#"{"id":18446744073709551615}"#, "").unwrap_err();
    let message = error.to_string();
    assert!(message.contains("/id"), "{message}");
    assert!(message.contains("did not match"), "{message}");
}

#[test]
fn stdout_json_decimal_expectation_does_not_match_integer_token() {
    // The decimal expectation equals the `f64` that `18446744073709551617`
    // rounds to, so only exact kind/token awareness keeps the comparison sound.
    let assertion = json_text_assertion("/id", "1.8446744073709552e19");

    // Positive control: a decimal token with that value matches.
    assert!(evaluate_assertion(&assertion, 0, r#"{"id":1.8446744073709552e19}"#, "").is_ok());

    // Negative: an integer token that rounds to the same `f64` must not match a
    // decimal expectation.
    let error =
        evaluate_assertion(&assertion, 0, r#"{"id":18446744073709551617}"#, "").unwrap_err();
    let message = error.to_string();
    assert!(message.contains("/id"), "{message}");
    assert!(message.contains("did not match"), "{message}");
}

#[test]
fn stdout_json_negative_zero_is_the_common_zero() {
    // Integer zero ignores the sign, as does decimal zero.
    let integer_zero = json_text_assertion("/value", "-0");
    assert!(evaluate_assertion(&integer_zero, 0, r#"{"value":0}"#, "").is_ok());
    assert!(evaluate_assertion(&integer_zero, 0, r#"{"value":-0}"#, "").is_ok());

    let decimal_zero = json_text_assertion("/value", "-0.0");
    assert!(evaluate_assertion(&decimal_zero, 0, r#"{"value":0.0}"#, "").is_ok());
    assert!(evaluate_assertion(&decimal_zero, 0, r#"{"value":-0e5}"#, "").is_ok());

    // The integer/decimal policy still separates `-0` from `-0.0`.
    let error = evaluate_assertion(&decimal_zero, 0, r#"{"value":0}"#, "").unwrap_err();
    assert!(error.to_string().contains("did not match"), "{error}");
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
fn stdout_json_accepts_big_integer_digits_in_string() {
    let assertion = json_assertion("/id", serde_json::json!("18446744073709551617"));

    assert!(evaluate_assertion(&assertion, 0, r#"{"id":"18446744073709551617"}"#, "").is_ok());
}

#[test]
fn canonical_decimal_normalizes_equivalent_forms() {
    let canonical = |token: &str| CanonicalDecimal::parse(token).unwrap();

    let one_and_a_half = canonical("1.5");
    assert_eq!(one_and_a_half, canonical("1.50"));
    assert_eq!(one_and_a_half, canonical("15e-1"));
    assert_eq!(one_and_a_half, canonical("0.15E1"));
    assert_eq!(one_and_a_half, canonical("1500e-3"));
    assert_ne!(one_and_a_half, canonical("1.5000000000000002"));

    assert_eq!(canonical("0"), canonical("-0"));
    assert_eq!(canonical("0.00"), canonical("-0.0E5"));
}

#[test]
fn canonical_decimal_rejects_malformed_tokens() {
    for token in [
        "", "-", "1.", ".5", "01", "1e", "1e+", "+1", "1.5.0", "1e1e1",
    ] {
        assert!(
            CanonicalDecimal::parse(token).is_err(),
            "{token:?} unexpectedly parsed"
        );
    }
}

#[test]
fn find_json_number_tokens_reports_every_number() {
    let found = find_json_number_tokens(
        r#"{"a":[1,1.5],"b":{"c":-0.0},"d":true,"e":"7","f":18446744073709551616}"#,
    )
    .unwrap();

    let pointers: Vec<(&str, &str)> = found
        .iter()
        .map(|number| (number.pointer.as_str(), number.token.as_str()))
        .collect();
    assert_eq!(
        pointers,
        vec![
            ("/a/0", "1"),
            ("/a/1", "1.5"),
            ("/b/c", "-0.0"),
            ("/f", "18446744073709551616"),
        ]
    );
}

#[test]
fn find_json_number_tokens_escapes_pointer_tokens() {
    let found = find_json_number_tokens(r#"{"a/b":{"c~d":1.5}}"#).unwrap();

    assert_eq!(found.len(), 1);
    assert_eq!(found[0].pointer, "/a~1b/c~0d");
    assert_eq!(found[0].token, "1.5");
}

#[test]
fn find_json_number_tokens_decodes_string_escapes_in_keys() {
    let found = find_json_number_tokens(r#"{"\u0061":18446744073709551616}"#).unwrap();

    assert_eq!(found.len(), 1);
    assert_eq!(found[0].pointer, "/a");
}
