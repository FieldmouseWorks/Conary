// apps/conary-test/src/engine/variables/tests.rs

#![cfg(test)]

use super::*;
use crate::config::distro::{
    DistroConfig, FixtureConfig, GlobalConfig, PathsConfig, RemiConfig, SetupConfig, TestPackage,
};
use crate::config::manifest::{TestDef, TestStep};

fn test_config() -> GlobalConfig {
    let mut distros = HashMap::new();
    distros.insert(
        "fedora44".to_string(),
        DistroConfig {
            remi_distro: "fedora-44".to_string(),
            repo_name: "remi-fedora-44".to_string(),
            build_context: crate::config::DistroBuildContext::Binary,
            containerfile: None,
            test_packages: vec![TestPackage {
                package: "conary-test-fixture".to_string(),
                binary: "/usr/bin/true".to_string(),
            }],
            release_root: None,
            target_root: None,
        },
    );

    GlobalConfig {
        remi: RemiConfig {
            endpoint: "https://remi.conary.io".to_string(),
        },
        paths: PathsConfig {
            db: "/tmp/conary-test.db".to_string(),
            conary_bin: "/usr/local/bin/conary".to_string(),
            test_hooks_conary_bin: Some("/usr/local/libexec/conary-test-hooks".to_string()),
            results_dir: "/tmp/results".to_string(),
            fixture_dir: Some("/opt/remi-tests/fixtures".to_string()),
        },
        setup: SetupConfig::default(),
        distros,
        fixtures: Some(FixtureConfig {
            package: Some("conary-test-fixture".to_string()),
            file: Some("/usr/share/conary-test/hello.txt".to_string()),
            added_file: Some("/usr/share/conary-test/added.txt".to_string()),
            marker: Some("/var/lib/conary-test/installed".to_string()),
            v1_version: Some("1.0.0".to_string()),
            v1_ccs_file: Some("conary-test-fixture-1.0.0-1.ccs".to_string()),
            v1_hello_sha256: Some(
                "18933c865fcf7230f8ea99b059747facc14285b7ed649758115f9c9a73f42a53".to_string(),
            ),
            v2_version: Some("2.0.0".to_string()),
            v2_ccs_file: Some("conary-test-fixture-2.0.0-1.ccs".to_string()),
            v2_hello_sha256: Some(
                "bd80c5e8a7138bd13d0f10e1358bda6f9727c266b6909d4b6c9293ab141ec1db".to_string(),
            ),
            v2_added_sha256: Some(
                "9767b0b4d55db9aee6638c9875b5cefea50c952cc77fbc5703ebc866b0daba3c".to_string(),
            ),
        }),
    }
}

#[test]
fn test_basic_expansion() {
    let mut vars = HashMap::new();
    vars.insert("NAME".to_string(), "world".to_string());
    assert_eq!(expand_variables("hello ${NAME}", &vars), "hello world");
}

#[test]
fn test_missing_variable_left_as_is() {
    let vars = HashMap::new();
    assert_eq!(
        expand_variables("hello ${MISSING}", &vars),
        "hello ${MISSING}"
    );
}

#[test]
fn test_empty_template() {
    let vars = HashMap::new();
    assert_eq!(expand_variables("", &vars), "");
}

#[test]
fn test_no_variables_in_input() {
    let mut vars = HashMap::new();
    vars.insert("KEY".to_string(), "value".to_string());
    assert_eq!(expand_variables("no vars here", &vars), "no vars here");
}

#[test]
fn test_multiple_variables() {
    let mut vars = HashMap::new();
    vars.insert("A".to_string(), "1".to_string());
    vars.insert("B".to_string(), "2".to_string());
    assert_eq!(expand_variables("${A} and ${B}", &vars), "1 and 2");
}

#[test]
fn expansion_replaces_adjacent_and_repeated_references() {
    let vars = HashMap::from([
        ("KEY".to_string(), "value".to_string()),
        ("A".to_string(), "1".to_string()),
        ("B".to_string(), "2".to_string()),
    ]);

    assert_eq!(expand_variables("${KEY}", &vars), "value");
    assert_eq!(expand_variables("${A}${B}", &vars), "12");
    assert_eq!(expand_variables("${KEY}/${KEY}", &vars), "value/value");
    assert_eq!(
        expand_variables("pre${A}mid${B}post", &vars),
        "pre1mid2post"
    );
}

#[test]
fn unknown_and_malformed_markers_are_left_verbatim() {
    let vars = HashMap::from([("KEY".to_string(), "value".to_string())]);

    // Unknown references stay visible for the unresolved-reference checks.
    assert_eq!(expand_variables("${NOPE}", &vars), "${NOPE}");
    // A `$` without `{` is not a marker.
    assert_eq!(expand_variables("$KEY", &vars), "$KEY");
    assert_eq!(expand_variables("cost $5", &vars), "cost $5");
    // Unterminated or otherwise malformed markers survive verbatim.
    assert_eq!(expand_variables("${", &vars), "${");
    assert_eq!(expand_variables("tail ${", &vars), "tail ${");
    assert_eq!(expand_variables("${KEY", &vars), "${KEY");
    assert_eq!(expand_variables("${KEY}${", &vars), "value${");
}

#[test]
fn shell_parameter_expansions_are_not_manifest_references() {
    // A shell expansion whose prefix parses as a known name must not expand
    // partially: the trailing `:-`, `#`, or `##` makes the marker malformed.
    let vars = HashMap::from([
        ("GEN".to_string(), "expanded".to_string()),
        ("NATIVE_PKG_SHA256".to_string(), "sha".to_string()),
        ("current_generation".to_string(), "gen".to_string()),
        ("KEY".to_string(), "value".to_string()),
    ]);

    assert_eq!(expand_variables("${GEN:-0}", &vars), "${GEN:-0}");
    assert_eq!(
        expand_variables("${#NATIVE_PKG_SHA256}", &vars),
        "${#NATIVE_PKG_SHA256}"
    );
    assert_eq!(
        expand_variables("${current_generation##*/}", &vars),
        "${current_generation##*/}"
    );
    // A well-formed reference after a malformed marker still expands.
    assert_eq!(
        expand_variables("${GEN:-0} ${KEY}", &vars),
        "${GEN:-0} value"
    );
}

#[test]
fn contains_variable_reference_reports_well_formed_and_malformed_markers() {
    assert!(contains_variable_reference("${KEY}"));
    assert!(contains_variable_reference("/data/${"));
    assert!(contains_variable_reference("${#VAR}"));
    assert!(!contains_variable_reference("$KEY"));
    assert!(!contains_variable_reference("plain text"));
}

#[test]
fn nested_template_in_a_value_is_emitted_verbatim() {
    // The finding's example: JSON_POINTER's value contains `${KEY}`. A
    // single-pass expander emits that value verbatim instead of rescanning it,
    // so `KEY` never enters the result.
    let vars = HashMap::from([
        ("JSON_POINTER".to_string(), "/data/${KEY}".to_string()),
        ("KEY".to_string(), "value".to_string()),
    ]);

    assert_eq!(expand_variables("${JSON_POINTER}", &vars), "/data/${KEY}");
}

#[test]
fn expansion_is_deterministic_across_map_insertion_orders() {
    let entries = [
        ("JSON_POINTER".to_string(), "/data/${KEY}".to_string()),
        ("KEY".to_string(), "value".to_string()),
        ("OTHER".to_string(), "other".to_string()),
    ];

    for iteration in 0..50 {
        let mut vars = HashMap::new();
        // Rotate the insertion order each iteration so different hash-table
        // layouts are exercised.
        for (key, value) in entries.iter().cycle().skip(iteration).take(entries.len()) {
            vars.insert(key.clone(), value.clone());
        }
        let expanded = expand_variables("${JSON_POINTER}", &vars);
        assert_eq!(expanded, "/data/${KEY}", "iteration {iteration}");
    }
}

#[test]
fn test_build_variables_populates_core_fields() {
    let config = test_config();
    let vars = build_variables(&config, "fedora44");

    assert_eq!(vars["REMI_ENDPOINT"], "https://remi.conary.io");
    assert_eq!(vars["DB_PATH"], "/tmp/conary-test.db");
    assert_eq!(vars["CONARY_BIN"], "/usr/local/bin/conary");
    assert_eq!(
        vars["CONARY_HOOKS_BIN"],
        "/usr/local/libexec/conary-test-hooks"
    );
    assert_eq!(vars["DISTRO"], "fedora44");
    assert_eq!(vars["REMI_DISTRO"], "fedora-44");
    assert_eq!(vars["REPO_NAME"], "remi-fedora-44");
    assert_eq!(vars["TEST_PACKAGE_1"], "conary-test-fixture");
    assert_eq!(vars["TEST_BINARY_1"], "/usr/bin/true");
    assert_eq!(vars["FIXTURE_PKG_NAME"], "conary-test-fixture");
    assert_eq!(
        vars["FIXTURE_CCS_KEY"],
        "/opt/remi-tests/fixtures/ccs-test-authority/fixture-signing-key.private"
    );
    assert_eq!(
        vars["FIXTURE_CCS_PUBLIC_KEY"],
        "/opt/remi-tests/fixtures/ccs-test-authority/fixture-signing-key.public"
    );
    assert_eq!(
        vars["FIXTURE_CCS_POLICY"],
        "/opt/remi-tests/fixtures/ccs-test-authority/trust-policy.toml"
    );
    assert_eq!(
        vars["FIXTURE_CCS_EXPIRED_POLICY"],
        "/opt/remi-tests/fixtures/ccs-test-authority/expired-trust-policy.toml"
    );
}

#[test]
fn test_hooks_binary_defaults_to_the_ordinary_integration_binary() {
    let mut config = test_config();
    config.paths.test_hooks_conary_bin = None;

    let vars = build_variables(&config, "fedora44");

    assert_eq!(vars["CONARY_HOOKS_BIN"], vars["CONARY_BIN"]);
}

#[test]
fn test_build_variables_fixture_ccs_paths() {
    let config = test_config();
    let vars = build_variables(&config, "fedora44");

    assert_eq!(
        vars["FIXTURE_V1_CCS"],
        "/opt/remi-tests/fixtures/conary-test-fixture/v1/output/conary-test-fixture-1.0.0-1.ccs"
    );
    assert_eq!(
        vars["FIXTURE_V2_CCS"],
        "/opt/remi-tests/fixtures/conary-test-fixture/v2/output/conary-test-fixture-2.0.0-1.ccs"
    );
}

#[test]
fn test_build_variables_unknown_distro() {
    let config = test_config();
    let vars = build_variables(&config, "unknown-distro");

    // Core fields still present.
    assert_eq!(vars["REMI_ENDPOINT"], "https://remi.conary.io");
    assert_eq!(vars["DISTRO"], "unknown-distro");
    // Distro-specific fields absent.
    assert!(!vars.contains_key("REMI_DISTRO"));
    assert!(!vars.contains_key("TEST_PACKAGE_1"));
}

#[test]
fn distro_expands_inside_quoted_manifest_commands() {
    let config = test_config();
    let vars = build_variables(&config, "linux-mint-22.3");

    assert_eq!(
        expand_variables(
            "python3 -c 'assert value[\"target_profile\"] == \"${DISTRO}\"'",
            &vars,
        ),
        "python3 -c 'assert value[\"target_profile\"] == \"linux-mint-22.3\"'"
    );
}

#[test]
fn test_distro_override_precedence() {
    let config = test_config();
    let mut vars = build_variables(&config, "fedora44");

    // Simulate a manifest that overrides REMI_ENDPOINT.
    let mut manifest = TestManifest {
        suite: crate::config::manifest::SuiteDef {
            name: "test".to_string(),
            phase: 1,
            setup: Vec::new(),
            mock_server: None,
            timeout: None,
            corpus: None,
        },
        test: Vec::new(),
        distro_overrides: HashMap::new(),
    };
    manifest.distro_overrides.insert(
        "fedora44".to_string(),
        HashMap::from([("REMI_ENDPOINT".to_string(), "http://override".to_string())]),
    );

    load_manifest_overrides(&mut vars, &manifest, "fedora44");
    assert_eq!(vars["REMI_ENDPOINT"], "http://override");
}

#[test]
fn build_manifest_variables_merges_distro_overrides_over_base_variables() {
    let config = test_config();
    let mut manifest = TestManifest {
        suite: crate::config::manifest::SuiteDef {
            name: "test".to_string(),
            phase: 1,
            setup: Vec::new(),
            mock_server: None,
            timeout: None,
            corpus: None,
        },
        test: Vec::new(),
        distro_overrides: HashMap::new(),
    };
    manifest.distro_overrides.insert(
        "fedora44".to_string(),
        HashMap::from([("REMI_ENDPOINT".to_string(), "http://override".to_string())]),
    );

    let vars = build_manifest_variables(&config, "fedora44", &manifest);

    assert_eq!(vars["DISTRO"], "fedora44");
    assert_eq!(vars["REMI_DISTRO"], "fedora-44");
    assert_eq!(vars["REMI_ENDPOINT"], "http://override");
}

#[test]
fn test_expand_assertion_substitutes_vars() {
    let mut vars = HashMap::new();
    vars.insert("PKG".to_string(), "conary-test-fixture".to_string());
    vars.insert("HELLO_SHA".to_string(), "abc123".to_string());

    let assertion = Assertion {
        stdout_contains_all: Some(vec!["${PKG}".to_string(), "Version".to_string()]),
        stderr_contains: Some("${PKG}".to_string()),
        stderr_not_contains: Some("${PKG}-panic".to_string()),
        file_checksum: Some(FileChecksum {
            path: "/tmp/${PKG}".to_string(),
            sha256: "${HELLO_SHA}".to_string(),
        }),
        ..Assertion::default()
    };

    let expanded = expand_assertion(&assertion, &vars);
    assert_eq!(
        expanded.stdout_contains_all,
        Some(vec![
            "conary-test-fixture".to_string(),
            "Version".to_string()
        ])
    );
    assert_eq!(
        expanded.stderr_contains.as_deref(),
        Some("conary-test-fixture")
    );
    assert_eq!(
        expanded.stderr_not_contains.as_deref(),
        Some("conary-test-fixture-panic")
    );
    assert_eq!(
        expanded.file_checksum.as_ref().map(|chk| chk.path.as_str()),
        Some("/tmp/conary-test-fixture")
    );
    assert_eq!(
        expanded
            .file_checksum
            .as_ref()
            .map(|chk| chk.sha256.as_str()),
        Some("abc123")
    );
}

#[test]
fn test_expand_assertion_expands_stdout_json_string_leaves() {
    let mut vars = HashMap::new();
    vars.insert("FOO".to_string(), "bar".to_string());

    let assertion = Assertion {
        stdout_json: Some(vec![JsonAssertion {
            pointer: "/data/${FOO}".to_string(),
            expected: JsonExpectation::Equals(serde_json::json!({
                "${FOO}": "${FOO}",
                "nested": ["${FOO}", { "leaf": "${FOO}" }],
                "number": 1,
                "flag": true,
                "nothing": null,
            })),
            numbers: HashMap::new(),
        }]),
        ..Assertion::default()
    };

    let expanded = expand_assertion(&assertion, &vars);
    let checks = expanded.stdout_json.as_ref().unwrap();
    assert_eq!(checks[0].pointer, "/data/bar");
    assert_eq!(
        checks[0].expected,
        JsonExpectation::Equals(serde_json::json!({
            "${FOO}": "bar",
            "nested": ["bar", { "leaf": "bar" }],
            "number": 1,
            "flag": true,
            "nothing": null,
        }))
    );
}

#[test]
fn test_expand_assertion_expands_null_stdout_json_pointer() {
    let mut vars = HashMap::new();
    vars.insert("FOO".to_string(), "bar".to_string());

    let assertion = Assertion {
        stdout_json: Some(vec![JsonAssertion {
            pointer: "/data/${FOO}".to_string(),
            expected: JsonExpectation::Null,
            numbers: HashMap::new(),
        }]),
        ..Assertion::default()
    };

    let expanded = expand_assertion(&assertion, &vars);
    let checks = expanded.stdout_json.as_ref().unwrap();
    assert_eq!(checks[0].pointer, "/data/bar");
    assert_eq!(checks[0].expected, JsonExpectation::Null);
}

#[test]
fn test_expand_qemu_boot_substitutes_vars() {
    let mut vars = HashMap::new();
    vars.insert("IMG".to_string(), "minimal-boot-v1".to_string());

    let expanded = expand_qemu_boot(
        &QemuBoot {
            image: "${IMG}".to_string(),
            local_image_path: None,
            image_format: crate::config::manifest::QemuImageFormat::Qcow2,
            stage_conary: false,
            scratch_disk_mb: None,
            copy_to_guest: Vec::new(),
            copy_from_guest: Vec::new(),
            memory_mb: 1024,
            timeout_seconds: 120,
            ssh_port: 2222,
            commands: vec!["echo ${IMG}".to_string()],
            expect_output: vec!["${IMG}".to_string()],
        },
        &vars,
    );

    assert_eq!(expanded.image, "minimal-boot-v1");
    assert_eq!(
        expanded.image_format,
        crate::config::manifest::QemuImageFormat::Qcow2
    );
    assert_eq!(expanded.commands, vec!["echo minimal-boot-v1"]);
    assert_eq!(expanded.expect_output, vec!["minimal-boot-v1"]);
}

#[test]
fn test_expand_qemu_boot_expands_local_image_and_copy_fields() {
    let mut vars = HashMap::new();
    vars.insert("IMG".to_string(), "minimal-boot-v2".to_string());
    vars.insert(
        "HOST_OUT".to_string(),
        "/tmp/conary-generation-export/generated.qcow2".to_string(),
    );
    vars.insert(
        "FIXTURE_ROOT".to_string(),
        "/opt/remi-tests/fixtures".to_string(),
    );

    let expanded = expand_qemu_boot(
        &QemuBoot {
            image: "${IMG}".to_string(),
            local_image_path: Some("${HOST_OUT}".to_string()),
            image_format: crate::config::manifest::QemuImageFormat::Qcow2,
            stage_conary: true,
            scratch_disk_mb: Some(4096),
            copy_to_guest: vec![QemuGuestCopy {
                source: "${FIXTURE_ROOT}/supported-host-generation-export".to_string(),
                dest: "/var/lib/conary/${IMG}".to_string(),
            }],
            copy_from_guest: vec![QemuGuestCopy {
                source: "/tmp/${IMG}.qcow2".to_string(),
                dest: "${HOST_OUT}".to_string(),
            }],
            memory_mb: 1024,
            timeout_seconds: 120,
            ssh_port: 2222,
            commands: vec!["test -s ${HOST_OUT}".to_string()],
            expect_output: vec!["${IMG}".to_string()],
        },
        &vars,
    );

    assert_eq!(expanded.image, "minimal-boot-v2");
    assert_eq!(
        expanded.local_image_path.as_deref(),
        Some("/tmp/conary-generation-export/generated.qcow2")
    );
    assert!(expanded.stage_conary);
    assert_eq!(expanded.scratch_disk_mb, Some(4096));
    assert_eq!(
        expanded.copy_to_guest[0].source,
        "/opt/remi-tests/fixtures/supported-host-generation-export"
    );
    assert_eq!(
        expanded.copy_to_guest[0].dest,
        "/var/lib/conary/minimal-boot-v2"
    );
    assert_eq!(
        expanded.copy_from_guest[0].source,
        "/tmp/minimal-boot-v2.qcow2"
    );
    assert_eq!(
        expanded.copy_from_guest[0].dest,
        "/tmp/conary-generation-export/generated.qcow2"
    );
    assert_eq!(
        expanded.commands,
        vec!["test -s /tmp/conary-generation-export/generated.qcow2"]
    );
}

#[test]
fn corpus_target_facts_expand_from_explicit_distro_overrides() {
    let definition: CorpusCaseDef = toml::from_str(
        r#"
evidence_path = "/tmp/evidence.json"
source_profile = "arch"
source_format = "${native_corpus_source_format}"
digest_source = "fixture_build_manifest"
stages = ["installation"]

[[coverage]]
semantic = "identity_exact_version"
artifact_roles = ["install_request"]

[target]
architecture = "x86_64"
init_system = "${target_init_system}"
capabilities = ["native_lifecycle", "${target_service_capability}"]
"#,
    )
    .unwrap();
    let vars = HashMap::from([
        (
            "native_corpus_source_format".to_string(),
            "alpm".to_string(),
        ),
        ("target_init_system".to_string(), "openrc".to_string()),
        (
            "target_service_capability".to_string(),
            "openrc_activation".to_string(),
        ),
    ]);

    let expanded = expand_corpus_case(&definition, &vars);
    assert_eq!(expanded.source_format.as_str(), "alpm");
    assert_eq!(expanded.target.init_system, "openrc");
    assert_eq!(
        expanded.target.capabilities,
        ["native_lifecycle", "openrc_activation"]
    );
}

/// A one-step test whose only assertion checks `pointer` via `stdout_json`,
/// matching the runner preflight fixtures.
fn stdout_json_pointer_test(pointer: &str) -> TestDef {
    TestDef {
        id: "TVAR01".to_string(),
        name: "variable preflight".to_string(),
        description: "variable expansion preflight".to_string(),
        timeout: 30,
        flaky: None,
        retries: None,
        retry_delay_ms: None,
        step: vec![TestStep {
            run: Some("echo ok".to_string()),
            assert: Some(Assertion {
                stdout_json: Some(vec![JsonAssertion {
                    pointer: pointer.to_string(),
                    expected: JsonExpectation::Equals(serde_json::json!(1)),
                    numbers: HashMap::new(),
                }]),
                ..Assertion::default()
            }),
            ..TestStep::default()
        }],
        resources: None,
        depends_on: None,
        fatal: None,
        group: None,
        skip: None,
        requires: Vec::new(),
        corpus: None,
    }
}

#[test]
fn nested_template_pointer_is_rejected_by_the_unresolved_reference_preflight() {
    let test = stdout_json_pointer_test("${JSON_POINTER}");

    // Positive control through the same fixture: a flat value passes the
    // preflight, so the rejection below can only come from the nested template.
    let flat = HashMap::from([
        ("JSON_POINTER".to_string(), "/data/value".to_string()),
        ("KEY".to_string(), "value".to_string()),
    ]);
    assert!(crate::engine::runner::preflight_stdout_json_pointers(&test, &flat).is_ok());

    // Negative: JSON_POINTER's value contains `${KEY}`, which the single-pass
    // expander emits literally, leaving an unresolved reference.
    let nested = HashMap::from([
        ("JSON_POINTER".to_string(), "/data/${KEY}".to_string()),
        ("KEY".to_string(), "value".to_string()),
    ]);
    assert_eq!(expand_variables("${JSON_POINTER}", &nested), "/data/${KEY}");
    let error = crate::engine::runner::preflight_stdout_json_pointers(&test, &nested)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("unresolved variable reference"),
        "error should report the unresolved reference: {error}"
    );
}
