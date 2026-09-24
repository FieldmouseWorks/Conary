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

/// Parse a single-key TOML snippet (`v = ...`) into its value.
fn toml_value(source: &str) -> toml::Value {
    let table: toml::Table = toml::from_str(source).unwrap();
    table["v"].clone()
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
    assert_eq!(checks[0].equals, toml::Value::String("planned".to_string()));
    assert_eq!(checks[1].pointer, "/data/removable");
    assert_eq!(
        checks[1].equals,
        toml_value(r#"v = [{ name = "dep-app", version = "1.0.0", architecture = "x86_64" }]"#)
    );
    assert_eq!(checks[2].pointer, "/data/skipped");
    assert_eq!(checks[2].equals, toml::Value::Array(Vec::new()));
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
