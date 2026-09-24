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
