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

/// Parse a `stdout_json` entry, asserting the manifest is rejected.
///
/// A known-good entry must parse in the same template first, so a rejection
/// can only come from the entry under test and never from template syntax.
fn stdout_json_entry_is_rejected(entry: &str) {
    assert!(
        toml::from_str::<TestManifest>(&stdout_json_manifest_source(
            r#"pointer = "/x", equals = 1"#
        ))
        .is_ok(),
        "positive control must parse"
    );
    assert!(toml::from_str::<TestManifest>(&stdout_json_manifest_source(entry)).is_err());
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
