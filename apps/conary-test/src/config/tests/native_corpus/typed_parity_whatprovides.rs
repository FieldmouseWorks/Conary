// apps/conary-test/src/config/tests/native_corpus/typed_parity_whatprovides.rs
#![cfg(test)]

use super::super::{load_manifest, remi_manifest_path};
use crate::config::manifest::{Assertion, JsonExpectation};
use crate::engine::{assertions::evaluate_assertion, variables::expand_assertion};
use serde_json::{Value, json};
use std::collections::HashMap;

const PACKAGE: &str = "phase4-runtime-fixture";
const QUERY: &str = "query whatprovides phase4-runtime-fixture --json";
const LANES: [(&str, &str, &str); 3] = [
    ("fedora44", "x86_64", "rpm"),
    ("ubuntu-26.04", "amd64", "debian"),
    ("arch", "x86_64", "arch"),
];

#[test]
fn native_parity_whatprovides_requires_exact_typed_result_for_each_lane() {
    let manifest = load_manifest(&remi_manifest_path("phase4-native-pm-parity.toml"))
        .expect("load native parity manifest");
    let test = manifest
        .test
        .iter()
        .find(|test| test.id == "TNPM05")
        .expect("TNPM05 daily queries");
    assert_eq!(test.step.len(), 5);
    let step = &test.step[3];
    assert_eq!(step.conary.as_deref(), Some(QUERY));
    assert!(step.run.is_none());

    for (distro, architecture, scheme) in LANES {
        let vars = manifest
            .distro_overrides
            .get(distro)
            .unwrap_or_else(|| panic!("missing {distro} overrides"));
        assert_eq!(lane_value(vars, "native_fixture_version"), "1.0.0-1");
        assert_eq!(lane_value(vars, "native_arch"), architecture);
        assert_eq!(lane_value(vars, "native_scheme"), scheme);

        let assertion = expand_assertion(step.assert.as_ref().expect("TNPM05 assertion"), vars);
        let expected = expected_result(vars);
        assert_exact_assertion(&assertion, &expected);
        reject_bad_results(&assertion, &expected, distro);
    }
}

fn lane_value<'a>(vars: &'a HashMap<String, String>, key: &str) -> &'a str {
    vars.get(key)
        .unwrap_or_else(|| panic!("missing manifest variable {key}"))
}

fn expected_result(vars: &HashMap<String, String>) -> Value {
    let version = lane_value(vars, "native_fixture_version");
    json!({
        "schema_version": 1,
        "capability": PACKAGE,
        "providers": [{
            "source_kind": "installed",
            "package": {
                "name": PACKAGE,
                "version": version,
                "release": "1",
                "architecture": lane_value(vars, "native_arch"),
                "version_scheme": lane_value(vars, "native_scheme"),
            },
            "repository": null,
            "capability_versions": [version],
        }],
        "provider_count": 1,
    })
}

fn assert_exact_assertion(assertion: &Assertion, expected: &Value) {
    assert_eq!(assertion.exit_code, Some(0));
    assert!(assertion.exit_code_not.is_none());
    assert!(assertion.stdout_contains.is_none());
    assert!(assertion.stdout_contains_all.is_none());
    assert!(assertion.stdout_contains_any.is_none());
    assert!(assertion.stdout_not_contains.is_none());
    assert!(assertion.stdout_contains_if_success.is_none());
    assert!(assertion.stdout_contains_any_if_success.is_none());
    let checks = assertion.stdout_json.as_ref().expect("JSON output check");
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].pointer, "");
    assert_eq!(
        checks[0].expected,
        JsonExpectation::Equals(expected.clone())
    );
    assert!(evaluate_assertion(assertion, 0, &expected.to_string(), "").is_ok());
}

fn reject_bad_results(assertion: &Assertion, expected: &Value, distro: &str) {
    let version = expected["providers"][0]["package"]["version"]
        .as_str()
        .expect("fixture version");
    for (pointer, replacement, defect) in [
        ("/schema_version", json!(2), "wrong schema"),
        ("/schema_version", json!("1"), "string schema"),
        ("/schema_version", json!(1.0), "decimal schema"),
        ("/capability", json!("other-capability"), "wrong capability"),
        ("/providers", json!({}), "provider array type"),
        (
            "/providers/0/source_kind",
            json!("repository"),
            "wrong source",
        ),
        (
            "/providers/0/package/name",
            json!("phase4-runtime-fixture-extra"),
            "near-name package",
        ),
        (
            "/providers/0/package/version",
            json!("9.0"),
            "wrong version",
        ),
        ("/providers/0/package/release", json!("2"), "wrong release"),
        ("/providers/0/package/release", json!(1), "release type"),
        (
            "/providers/0/package/architecture",
            json!("wrong-architecture"),
            "wrong architecture",
        ),
        (
            "/providers/0/package/version_scheme",
            json!("conary"),
            "wrong version scheme",
        ),
        (
            "/providers/0/repository",
            json!({"name": "unexpected"}),
            "unexpected repository",
        ),
        (
            "/providers/0/capability_versions",
            json!([]),
            "missing capability version",
        ),
        (
            "/providers/0/capability_versions",
            json!([version, "another-version"]),
            "extra capability version",
        ),
        (
            "/providers/0/capability_versions",
            json!(version),
            "capability versions type",
        ),
        ("/provider_count", json!(2), "wrong provider count"),
        ("/provider_count", json!("1"), "string provider count"),
        ("/provider_count", json!(1.0), "decimal provider count"),
    ] {
        let mut actual = expected.clone();
        *actual.pointer_mut(pointer).expect("control pointer") = replacement;
        rejects(assertion, &actual, distro, defect);
    }

    let mut extra = expected.clone();
    let mut near_name = expected["providers"][0].clone();
    near_name["package"]["name"] = json!("phase4-runtime-fixture-extra");
    extra["providers"]
        .as_array_mut()
        .expect("provider array")
        .push(near_name);
    extra["provider_count"] = json!(2);
    rejects(assertion, &extra, distro, "extra near-name provider");

    let mut extra_key = expected.clone();
    extra_key["unexpected"] = json!(true);
    rejects(assertion, &extra_key, distro, "extra root field");

    let valid = expected.to_string();
    rejects_stdout(assertion, "not JSON", distro, "malformed JSON");
    rejects_stdout(
        assertion,
        &format!("{valid}{valid}"),
        distro,
        "concatenated JSON",
    );
    assert!(
        evaluate_assertion(assertion, 1, &valid, "").is_err(),
        "{distro} must reject nonzero exit"
    );

    let legacy = Assertion {
        exit_code: Some(0),
        stdout_contains_all: Some(vec![
            format!("Capability '{PACKAGE}' is provided by:"),
            format!("{PACKAGE} {version}"),
        ]),
        ..Assertion::default()
    };
    let legacy_text = format!(
        "Capability '{PACKAGE}' is provided by:\n  {PACKAGE} {version}\n  {PACKAGE}-extra {version}\nTotal: 2 provider(s)"
    );
    assert!(
        evaluate_assertion(&legacy, 0, &legacy_text, "").is_ok(),
        "{distro} old substring assertion accepts an extra provider"
    );
}

fn rejects(assertion: &Assertion, actual: &Value, distro: &str, defect: &str) {
    rejects_stdout(assertion, &actual.to_string(), distro, defect);
}

fn rejects_stdout(assertion: &Assertion, stdout: &str, distro: &str, defect: &str) {
    assert!(
        evaluate_assertion(assertion, 0, stdout, "").is_err(),
        "{distro} must reject {defect}"
    );
}
