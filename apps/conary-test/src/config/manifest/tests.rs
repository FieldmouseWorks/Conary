// apps/conary-test/src/config/manifest/tests.rs

#![cfg(test)]

use super::*;

#[test]
fn qemu_boot_defaults_to_qcow2_image_format() {
    let manifest: TestManifest = toml::from_str(
        r#"
            [suite]
            name = "QEMU format"
            phase = 3

            [[test]]
            id = "TQEMU01"
            name = "default format"
            description = "default qemu image format"
            timeout = 1

            [[test.step]]
            [test.step.qemu_boot]
            image = "minimal-boot-v3"
            commands = ["true"]
            "#,
    )
    .unwrap();

    let qemu = manifest.test[0].step[0].qemu_boot.as_ref().unwrap();
    assert_eq!(qemu.image_format, QemuImageFormat::Qcow2);
}

#[test]
fn qemu_boot_parses_iso_image_format() {
    let manifest: TestManifest = toml::from_str(
        r#"
            [suite]
            name = "QEMU format"
            phase = 3

            [[test]]
            id = "TQEMU02"
            name = "iso format"
            description = "iso qemu image format"
            timeout = 1

            [[test.step]]
            [test.step.qemu_boot]
            image = "local-generation-iso"
            image_format = "iso"
            commands = ["true"]
            "#,
    )
    .unwrap();

    let qemu = manifest.test[0].step[0].qemu_boot.as_ref().unwrap();
    assert_eq!(qemu.image_format, QemuImageFormat::Iso);
}

#[test]
fn stdout_json_assertion_parses_pointer_and_value() {
    let manifest: TestManifest = toml::from_str(
        r#"
        [suite]
        name = "stdout-json"
        phase = 4

        [[test]]
        id = "TJSON01"
        name = "typed stdout json"
        description = "parses typed stdout_json checks"
        timeout = 10

        [[test.step]]
        run = "true"

        [test.step.assert]
        exit_code = 0
        stdout_json = [
          { pointer = "/status", equals = "planned" },
          { pointer = "/data/removable", equals = [{ name = "dep-app", version = "1.0.0", architecture = "x86_64" }] },
          { pointer = "/data/skipped", equals = [] },
        ]
        "#,
    )
    .unwrap();

    let assertion = manifest.test[0].step[0].assert.as_ref().unwrap();
    let checks = assertion.stdout_json.as_ref().unwrap();
    assert_eq!(checks.len(), 3);
    assert_eq!(checks[0].pointer, "/status");
    assert_eq!(
        checks[0].expected,
        JsonExpectation::Equals(serde_json::json!("planned"))
    );
    assert_eq!(checks[1].pointer, "/data/removable");
    assert_eq!(
        checks[1].expected,
        JsonExpectation::Equals(serde_json::json!([{
            "name": "dep-app",
            "version": "1.0.0",
            "architecture": "x86_64",
        }]))
    );
    assert_eq!(checks[2].pointer, "/data/skipped");
    assert_eq!(
        checks[2].expected,
        JsonExpectation::Equals(serde_json::json!([]))
    );
}

#[test]
fn stdout_json_assertion_parses_null() {
    let manifest: TestManifest = toml::from_str(
        r#"
        [suite]
        name = "stdout-json"
        phase = 4

        [[test]]
        id = "TJSON03"
        name = "null stdout json"
        description = "parses a null stdout_json check"
        timeout = 10

        [[test.step]]
        run = "true"

        [test.step.assert]
        stdout_json = [{ pointer = "/x", null = true }]
        "#,
    )
    .unwrap();

    let checks = manifest.test[0].step[0]
        .assert
        .as_ref()
        .unwrap()
        .stdout_json
        .as_ref()
        .unwrap();
    assert_eq!(checks[0].pointer, "/x");
    assert_eq!(checks[0].expected, JsonExpectation::Null);
}

/// Parse a single-entry `stdout_json` manifest and return its expectation.
fn stdout_json_expectation(entry: &str) -> JsonExpectation {
    let source = format!(
        r#"
        [suite]
        name = "stdout-json"
        phase = 4

        [[test]]
        id = "TJSON05"
        name = "valid stdout json"
        description = "parses a valid stdout_json entry"
        timeout = 10

        [[test.step]]
        run = "true"

        [test.step.assert]
        stdout_json = [{{ {entry} }}]
        "#
    );

    let manifest: TestManifest = toml::from_str(&source).unwrap();
    manifest.test[0].step[0]
        .assert
        .as_ref()
        .unwrap()
        .stdout_json
        .as_ref()
        .unwrap()[0]
        .expected
        .clone()
}

/// Parse a single-entry `stdout_json` manifest and return its pointer.
fn stdout_json_pointer(entry: &str) -> String {
    let manifest: TestManifest = toml::from_str(&stdout_json_manifest_source(entry)).unwrap();
    manifest.test[0].step[0]
        .assert
        .as_ref()
        .unwrap()
        .stdout_json
        .as_ref()
        .unwrap()[0]
        .pointer
        .clone()
}

/// Parse a single-entry `stdout_json` manifest and return its typed check.
fn stdout_json_check(entry: &str) -> JsonAssertion {
    let manifest: TestManifest = toml::from_str(&stdout_json_manifest_source(entry)).unwrap();
    manifest.test[0].step[0]
        .assert
        .as_ref()
        .unwrap()
        .stdout_json
        .as_ref()
        .unwrap()[0]
        .clone()
}

#[test]
fn stdout_json_accepts_valid_rfc6901_pointers() {
    let cases = [
        (r#"pointer = "", equals = 1"#, ""),
        (r#"pointer = "/", equals = 1"#, "/"),
        (r#"pointer = "/a~0b/c~1d", equals = 1"#, "/a~0b/c~1d"),
        (r#"pointer = "/data/${VAR}", equals = 1"#, "/data/${VAR}"),
    ];

    for (entry, expected) in cases {
        assert_eq!(stdout_json_pointer(entry), expected, "entry: {entry}");
    }
}

#[test]
fn stdout_json_defers_pointer_validation_for_variable_templates() {
    assert_eq!(
        stdout_json_pointer(r#"pointer = "${JSON_POINTER}", equals = 1"#),
        "${JSON_POINTER}"
    );
}

#[test]
fn stdout_json_equals_converts_toml_values_to_json() {
    let cases = [
        (r#"pointer = "/x", equals = 42"#, serde_json::json!(42)),
        (r#"pointer = "/x", equals = 1.5"#, serde_json::json!(1.5)),
        (
            r#"pointer = "/x", equals = "planned""#,
            serde_json::json!("planned"),
        ),
        (r#"pointer = "/x", equals = true"#, serde_json::json!(true)),
        (
            r#"pointer = "/x", equals = [1, "two", false]"#,
            serde_json::json!([1, "two", false]),
        ),
        (
            r#"pointer = "/x", equals = { name = "dep-app", version = "1.0.0" }"#,
            serde_json::json!({ "name": "dep-app", "version": "1.0.0" }),
        ),
    ];

    for (entry, expected) in cases {
        assert_eq!(
            stdout_json_expectation(entry),
            JsonExpectation::Equals(expected),
            "entry: {entry}"
        );
    }
}

#[test]
fn stdout_json_equals_json_keeps_full_range_unsigned_integer() {
    let expectation =
        stdout_json_expectation(r#"pointer = "/id", equals_json = '18446744073709551615'"#);

    // `serde_json` must keep the exact token as a `u64`; routing it through
    // `f64` or `i64` would lose or reject it.
    let JsonExpectation::Equals(value) = expectation else {
        panic!("expected an equals expectation");
    };
    assert_eq!(value.as_u64(), Some(u64::MAX));
}

#[test]
fn stdout_json_equals_json_round_trips_nested_null() {
    let expectation = stdout_json_expectation(r#"pointer = "/x", equals_json = '{"a":[1,null]}'"#);

    // TOML `equals` cannot express the nested null.
    assert_eq!(
        expectation,
        JsonExpectation::Equals(serde_json::json!({ "a": [1, null] }))
    );
}

#[test]
fn stdout_json_equals_json_accepts_signed_integer_minimum() {
    let expectation =
        stdout_json_expectation(r#"pointer = "/id", equals_json = '-9223372036854775808'"#);

    let JsonExpectation::Equals(value) = expectation else {
        panic!("expected an equals expectation");
    };
    assert_eq!(value.as_i64(), Some(i64::MIN));
}

#[test]
fn stdout_json_equals_json_accepts_big_integer_digits_in_a_string() {
    // A digit run inside a string is not a JSON number and must load.
    assert_eq!(
        stdout_json_expectation(r#"pointer = "/id", equals_json = '"18446744073709551616"'"#),
        JsonExpectation::Equals(serde_json::json!("18446744073709551616"))
    );
}

#[test]
fn stdout_json_equals_json_keeps_float_exponent_token() {
    // The parsed value is `f64`, but the exact token is kept for comparison.
    let check = stdout_json_check(r#"pointer = "/x", equals_json = '1.5e3'"#);
    assert_eq!(
        check.expected,
        JsonExpectation::Equals(serde_json::json!(1500.0))
    );
    assert_eq!(check.numbers.get(""), Some(&"1.5e3".to_string()));
}

#[test]
fn stdout_json_equals_json_keeps_out_of_range_integer_token() {
    // Positive control: `u64::MAX` fits `u64`, so it is exact without help.
    assert_eq!(
        stdout_json_expectation(r#"pointer = "/id", equals_json = '18446744073709551615'"#),
        JsonExpectation::Equals(serde_json::json!(u64::MAX))
    );

    // `u64::MAX + 1` only parses as `f64`, but the loader keeps the exact
    // source token so the comparator can use it instead of a rounded value.
    let check = stdout_json_check(r#"pointer = "/id", equals_json = '18446744073709551616'"#);
    assert_eq!(
        check.numbers.get(""),
        Some(&"18446744073709551616".to_string())
    );
}

#[test]
fn stdout_json_equals_json_keeps_out_of_range_negative_integer_token() {
    // Positive control: `i64::MIN` fits `i64` and is exact.
    assert_eq!(
        stdout_json_expectation(r#"pointer = "/id", equals_json = '-9223372036854775808'"#),
        JsonExpectation::Equals(serde_json::json!(i64::MIN))
    );

    // One below `i64::MIN` fits neither signed nor unsigned; the token is kept.
    let check = stdout_json_check(r#"pointer = "/id", equals_json = '-9223372036854775809'"#);
    assert_eq!(
        check.numbers.get(""),
        Some(&"-9223372036854775809".to_string())
    );
}

#[test]
fn stdout_json_equals_json_keeps_nested_out_of_range_integer_token() {
    let check =
        stdout_json_check(r#"pointer = "/data", equals_json = '{"id": 18446744073709551616}'"#);

    // The token is keyed by its pointer relative to the expected document.
    assert_eq!(
        check.numbers.get("/id"),
        Some(&"18446744073709551616".to_string())
    );
}

#[test]
fn stdout_json_equals_json_accepts_largest_finite_float_token() {
    // Boundary positive control: `f64::MAX` is finite and its decimal order is
    // exactly 308, so neither the structural nor the infinity check fires.
    let check = stdout_json_check(r#"pointer = "/x", equals_json = '1.7976931348623157e308'"#);

    let JsonExpectation::Equals(value) = check.expected else {
        panic!("expected an equals expectation");
    };
    assert_eq!(value.as_f64(), Some(f64::MAX));
}

#[test]
fn stdout_json_equals_json_rejects_number_beyond_f64_range() {
    // Positive control through the same fixture: `u64::MAX + 1` is finite in
    // `f64`, so the loader keeps its exact token and accepts the manifest.
    let finite = stdout_json_check(r#"pointer = "/id", equals_json = '18446744073709551616'"#);
    assert_eq!(
        finite.numbers.get(""),
        Some(&"18446744073709551616".to_string())
    );

    // Negative: a 400-digit integer exceeds finite `f64` range, which the
    // default `serde_json` parser cannot represent.
    let digits = "9".repeat(400);
    let error = stdout_json_entry_error(&format!(r#"pointer = "/id", equals_json = '{digits}'"#));

    assert!(error.contains("/id"), "{error}");
    assert!(error.contains("not supported"), "{error}");
    // The range limitation is named instead of serde_json's generic error.
    assert!(!error.contains("invalid `equals_json` JSON"), "{error}");
}

#[test]
fn stdout_json_equals_json_rejects_infinite_magnitude_number() {
    // Both detection branches: an exponent order above 308, and an order-308
    // value whose leading digits push it past `f64::MAX`.
    for token in ["1e309", "1.8e308"] {
        let error = stdout_json_entry_error(&format!(r#"pointer = "/x", equals_json = '{token}'"#));

        assert!(error.contains("/x"), "{token}: {error}");
        assert!(error.contains("not supported"), "{token}: {error}");
    }
}

#[test]
fn stdout_json_equals_json_rejects_out_of_i64_exponent() {
    // Positive control through the same fixture: an exponent that fits `i64`
    // loads and keeps its exact source token for the comparator.
    let check = stdout_json_check(r#"pointer = "/x", equals_json = '1e-300'"#);
    assert_eq!(check.numbers.get(""), Some(&"1e-300".to_string()));

    // Negative: the exponent does not fit `i64`, so `CanonicalDecimal::parse`
    // cannot canonicalize it even though `serde_json` reads the token as a
    // finite zero. The loader must refuse it instead of deferring the failure
    // to comparison.
    let entry = r#"pointer = "/x", equals_json = '1e-9223372036854775809'"#;
    let error = stdout_json_entry_error(entry);

    assert!(error.contains("/x"), "{error}");
    assert!(error.contains("not supported"), "{error}");
    assert!(!error.contains("invalid `equals_json` JSON"), "{error}");
}

#[test]
fn stdout_json_equals_has_no_json_source_tokens() {
    // TOML `equals` values have no JSON text, so the comparator reduces them
    // through the value's shortest round-trip form instead.
    let check = stdout_json_check(r#"pointer = "/x", equals = 1.5"#);
    assert!(check.numbers.is_empty());

    let check = stdout_json_check(r#"pointer = "/x", equals_json = '1.5'"#);
    assert_eq!(check.numbers.get(""), Some(&"1.5".to_string()));
}

/// Parse a `stdout_json` entry and return the manifest error message.
///
/// A known-good entry must parse in the same template first, so a rejection
/// can only come from the entry under test and never from template syntax.
fn stdout_json_entry_error(entry: &str) -> String {
    assert!(
        toml::from_str::<TestManifest>(&stdout_json_manifest_source(
            r#"pointer = "/x", equals = 1"#
        ))
        .is_ok(),
        "positive control must parse"
    );
    let result = toml::from_str::<TestManifest>(&stdout_json_manifest_source(entry));
    result.unwrap_err().to_string()
}

/// Parse a `stdout_json` entry, asserting the manifest is rejected.
fn stdout_json_entry_is_rejected(entry: &str) {
    stdout_json_entry_error(entry);
}

fn stdout_json_manifest_source(entry: &str) -> String {
    format!(
        r#"
        [suite]
        name = "stdout-json"
        phase = 4

        [[test]]
        id = "TJSON04"
        name = "bad stdout json"
        description = "rejects an invalid stdout_json entry"
        timeout = 10

        [[test.step]]
        run = "true"

        [test.step.assert]
        stdout_json = [{{ {entry} }}]
        "#
    )
}

#[test]
fn stdout_json_rejects_null_false() {
    stdout_json_entry_is_rejected(r#"pointer = "/x", null = false"#);
}

#[test]
fn stdout_json_rejects_both_equals_and_null() {
    stdout_json_entry_is_rejected(r#"pointer = "/x", equals = "y", null = true"#);
}

#[test]
fn stdout_json_rejects_invalid_equals_json_text() {
    let error = stdout_json_entry_error(r#"pointer = "/x", equals_json = "not json""#);

    assert!(error.contains("/x"), "{error}");
    assert!(error.contains("equals_json"), "{error}");
}

#[test]
fn stdout_json_rejects_equals_together_with_equals_json() {
    let error = stdout_json_entry_error(r#"pointer = "/x", equals = 1, equals_json = "1""#);

    assert!(error.contains("exactly one"), "{error}");
}

#[test]
fn stdout_json_rejects_equals_json_together_with_null() {
    let error = stdout_json_entry_error(r#"pointer = "/x", equals_json = "1", null = true"#);

    assert!(error.contains("exactly one"), "{error}");
}

#[test]
fn stdout_json_rejects_neither_equals_nor_null() {
    stdout_json_entry_is_rejected(r#"pointer = "/x""#);
}

#[test]
fn stdout_json_rejects_pointer_without_leading_slash() {
    stdout_json_entry_is_rejected(r#"pointer = "status", equals = 1"#);
}

#[test]
fn stdout_json_rejects_pointer_with_invalid_escape() {
    stdout_json_entry_is_rejected(r#"pointer = "/a~2b", equals = 1"#);
}

#[test]
fn stdout_json_rejects_pointer_with_trailing_tilde() {
    stdout_json_entry_is_rejected(r#"pointer = "/a~", equals = 1"#);
}

#[test]
fn stdout_json_rejects_datetime_equals() {
    stdout_json_entry_is_rejected(r#"pointer = "/x", equals = 1979-05-27T07:32:00Z"#);
}

#[test]
fn stdout_json_rejects_nan_equals() {
    stdout_json_entry_is_rejected(r#"pointer = "/x", equals = nan"#);
}

#[test]
fn stdout_json_rejects_inf_equals() {
    stdout_json_entry_is_rejected(r#"pointer = "/x", equals = inf"#);
}

#[test]
fn stdout_json_rejects_nested_non_finite_equals() {
    stdout_json_entry_is_rejected(r#"pointer = "/x", equals = { a = [nan] }"#);
}

#[test]
fn stdout_json_unknown_key_is_rejected() {
    let source = r#"
        [suite]
        name = "stdout-json"
        phase = 4

        [[test]]
        id = "TJSON02"
        name = "bad stdout json"
        description = "rejects an unknown stdout_json key"
        timeout = 10

        [[test.step]]
        run = "true"

        [test.step.assert]
        stdout_json = [{ pointr = "/x", equals = "y" }]
        "#;

    let error = toml::from_str::<TestManifest>(source).unwrap_err();
    assert!(error.to_string().contains("pointr"));
}

/// Parse a manifest whose single step asserts the given `stdout_json` array
/// entries. The entries string is the body of the `stdout_json` array.
fn stdout_json_checks_manifest(entries: &str) -> TestManifest {
    let source = format!(
        r#"
        [suite]
        name = "stdout-json"
        phase = 4

        [[test]]
        id = "TJSON06"
        name = "overlapping stdout json"
        description = "rejects overlapping stdout_json pointers"
        timeout = 10

        [[test.step]]
        run = "true"

        [test.step.assert]
        stdout_json = [{entries}]
        "#
    );

    toml::from_str(&source).unwrap()
}

#[test]
fn json_pointer_tokens_splits_reference_tokens() {
    assert_eq!(json_pointer_tokens(""), Vec::<&str>::new());
    assert_eq!(json_pointer_tokens("/"), vec![""]);
    assert_eq!(json_pointer_tokens("/a"), vec!["a"]);
    assert_eq!(json_pointer_tokens("/a/b"), vec!["a", "b"]);
    assert_eq!(json_pointer_tokens("/a~0b/c~1d"), vec!["a~0b", "c~1d"]);
}

#[test]
fn pointers_overlap_compares_reference_tokens() {
    let cases = [
        // Equal pointers.
        ("/a", "/a", true),
        // Proper ancestor and its descendant, in both orders.
        ("/a", "/a/b", true),
        ("/a/b", "/a", true),
        // Siblings constrain separate subtrees.
        ("/a/b", "/a/c", false),
        // A string prefix of a token is not an ancestor.
        ("/ab", "/a", false),
        ("/a", "/ab", false),
        // The root pointer is an ancestor of every other pointer.
        ("", "/status", true),
        ("", "", true),
        // Escaped tokens compare as tokens, so `~1` is not a separator.
        ("/a~1b", "/a/b", false),
    ];

    for (a, b, expected) in cases {
        assert_eq!(pointers_overlap(a, b), expected, "{a:?} vs {b:?}");
    }
}

#[test]
fn stdout_json_distinct_pointers_load() {
    let manifest = stdout_json_checks_manifest(
        r#"{ pointer = "/status", equals = 1 }, { pointer = "/data/value", equals = 2 }"#,
    );

    assert!(manifest.validate().is_ok());
}

#[test]
fn stdout_json_allows_non_overlapping_pointers_under_a_shared_parent() {
    // Positive: siblings constrain separate subtrees.
    let manifest = stdout_json_checks_manifest(
        r#"{ pointer = "/data/a", equals = 1 }, { pointer = "/data/b", equals = 2 }"#,
    );

    assert!(manifest.validate().is_ok());
}

#[test]
fn stdout_json_allows_pointer_that_is_a_string_prefix_only() {
    // Positive: `/ab` is a string prefix of `/a`, but neither pointer is an
    // ancestor of the other.
    let manifest = stdout_json_checks_manifest(
        r#"{ pointer = "/ab", equals = 1 }, { pointer = "/a", equals = 2 }"#,
    );

    assert!(manifest.validate().is_ok());
}

#[test]
fn stdout_json_rejects_ancestor_and_descendant_pointers() {
    // Positive control: the same helper and manifest shape load when the two
    // pointers constrain separate subtrees.
    assert!(
        stdout_json_checks_manifest(
            r#"{ pointer = "/data", equals = 1 }, { pointer = "/other/status", equals = 2 }"#,
        )
        .validate()
        .is_ok()
    );

    // Negative: `/data` is an ancestor of `/data/status`, so the ancestor
    // check already determines the descendant.
    let ancestor = stdout_json_checks_manifest(
        r#"{ pointer = "/data", equals = { status = "planned" } }, { pointer = "/data/status", equals = "failed" }"#,
    );
    assert!(ancestor.validate().is_err());

    // The reverse order is the same overlap.
    let descendant = stdout_json_checks_manifest(
        r#"{ pointer = "/data/status", equals = "failed" }, { pointer = "/data", equals = { status = "planned" } }"#,
    );
    assert!(descendant.validate().is_err());
}

#[test]
fn stdout_json_rejects_root_pointer_with_any_other() {
    // Positive control: the root pointer alone loads.
    assert!(
        stdout_json_checks_manifest(r#"{ pointer = "", equals = 1 }"#)
            .validate()
            .is_ok()
    );

    // Negative: the root pointer addresses the whole document, so it is an
    // ancestor of every other pointer.
    let manifest = stdout_json_checks_manifest(
        r#"{ pointer = "", equals = 1 }, { pointer = "/status", equals = 2 }"#,
    );

    assert!(manifest.validate().is_err());
}

#[test]
fn stdout_json_rejects_duplicate_pointers() {
    // Positive control: the same step with a distinct second pointer loads.
    assert!(
        stdout_json_checks_manifest(
            r#"{ pointer = "/status", equals = 1 }, { pointer = "/data/value", equals = 2 }"#,
        )
        .validate()
        .is_ok()
    );

    // Negative: a duplicate is the overlap case where the pointers are equal.
    let manifest = stdout_json_checks_manifest(
        r#"{ pointer = "/status", equals = 1 }, { pointer = "/status", equals = 2 }"#,
    );

    assert!(manifest.validate().is_err());
}

#[test]
fn stdout_json_rejects_duplicate_pointers_with_identical_expectations() {
    // Positive control: the same step with a distinct second pointer loads.
    assert!(
        stdout_json_checks_manifest(
            r#"{ pointer = "/status", equals = 1 }, { pointer = "/data/value", equals = 1 }"#,
        )
        .validate()
        .is_ok()
    );

    // Negative: equal pointers overlap even though both expectations agree.
    let manifest = stdout_json_checks_manifest(
        r#"{ pointer = "/status", equals = 1 }, { pointer = "/status", equals = 1 }"#,
    );

    assert!(manifest.validate().is_err());
}

/// Build a manifest whose single `suite.setup` step asserts the given
/// `stdout_json` array entries. `entries` is the body of the array.
fn suite_setup_stdout_json_source(entries: &str) -> String {
    format!(
        r#"
        [suite]
        name = "setup-stdout-json"
        phase = 4

        [[suite.setup]]
        run = "true"

        [suite.setup.assert]
        stdout_json = [{entries}]

        [[test]]
        id = "TSETUP01"
        name = "setup stdout json"
        description = "validates suite setup stdout_json"
        timeout = 10

        [[test.step]]
        run = "true"
        "#
    )
}

/// Write a suite-setup `stdout_json` manifest into `dir` and load it through
/// the same `load_manifest` path the CLI uses.
fn load_suite_setup_stdout_json(
    dir: &std::path::Path,
    file: &str,
    entries: &str,
) -> anyhow::Result<TestManifest> {
    let path = dir.join(file);
    std::fs::write(&path, suite_setup_stdout_json_source(entries)).unwrap();
    crate::config::load_manifest(&path)
}

#[test]
fn load_manifest_rejects_overlapping_suite_setup_stdout_json_pointers() {
    let dir = tempfile::tempdir().unwrap();

    // Positive control through the same fixture: sibling pointers under a
    // shared parent load, so the negative can only fail on the overlap rule.
    assert!(
        load_suite_setup_stdout_json(
            dir.path(),
            "siblings.toml",
            r#"{ pointer = "/data/a", equals = 1 }, { pointer = "/data/b", equals = 2 }"#,
        )
        .is_ok()
    );

    // Negative: `/data` is an ancestor of `/data/status`, so the ancestor
    // check already determines the descendant.
    let error = load_suite_setup_stdout_json(
        dir.path(),
        "overlap.toml",
        r#"{ pointer = "/data", equals = { status = "planned" } }, { pointer = "/data/status", equals = "failed" }"#,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("suite setup"), "{error}");
    assert!(error.contains("/data/status"), "{error}");
}

#[test]
fn load_manifest_rejects_malformed_suite_setup_literal_pointer() {
    let dir = tempfile::tempdir().unwrap();

    // Positive control through the same fixture: a valid literal pointer in
    // suite setup loads.
    assert!(
        load_suite_setup_stdout_json(
            dir.path(),
            "valid.toml",
            r#"{ pointer = "/data", equals = 1 }"#,
        )
        .is_ok()
    );

    // Negative: a literal pointer without a leading slash is rejected while
    // loading the manifest.
    let error = load_suite_setup_stdout_json(
        dir.path(),
        "malformed.toml",
        r#"{ pointer = "data", equals = 1 }"#,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("RFC 6901"), "{error}");
}

/// Build a manifest source with one `distro_overrides` entry for `fedora44`
/// whose variable name is `key`.
fn distro_override_manifest_source(key: &str) -> String {
    format!(
        r#"
        [suite]
        name = "distro-overrides"
        phase = 4

        [[test]]
        id = "TOVR01"
        name = "distro override key"
        description = "validates distro_override variable names"
        timeout = 10

        [[test.step]]
        run = "true"

        [distro_overrides.fedora44]
        "{key}" = "value"
        "#
    )
}

/// Write a one-override manifest into a temp dir and load it through
/// `load_manifest`, the same path the CLI uses.
fn load_distro_override(key: &str) -> anyhow::Result<TestManifest> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("distro-override.toml");
    std::fs::write(&path, distro_override_manifest_source(key)).unwrap();
    crate::config::load_manifest(&path)
}

#[test]
fn distro_override_accepts_template_name() {
    let manifest = load_distro_override("JSON_POINTER").unwrap();

    assert!(
        manifest
            .distro_overrides
            .get("fedora44")
            .unwrap()
            .contains_key("JSON_POINTER")
    );
}

/// Load `key` through the same fixture as the valid-name control, so a
/// rejection can only come from the variable-name rule.
fn distro_override_rejection(key: &str) -> String {
    assert!(
        load_distro_override("JSON_POINTER").is_ok(),
        "positive control must load"
    );
    load_distro_override(key).unwrap_err().to_string()
}

#[test]
fn distro_override_rejects_hyphenated_name() {
    let error = distro_override_rejection("JSON-POINTER");

    assert!(error.contains("manifest \"distro-overrides\""), "{error}");
    assert!(error.contains("fedora44"), "{error}");
    assert!(error.contains("JSON-POINTER"), "{error}");
}

#[test]
fn distro_override_rejects_leading_digit_name() {
    let error = distro_override_rejection("1KEY");

    assert!(error.contains("distro_overrides key \"1KEY\""), "{error}");
}

#[test]
fn distro_override_rejects_empty_name() {
    let error = distro_override_rejection("");

    assert!(error.contains("distro_overrides key \"\""), "{error}");
}
