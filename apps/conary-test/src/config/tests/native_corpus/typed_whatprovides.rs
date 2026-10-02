// apps/conary-test/src/config/tests/native_corpus/typed_whatprovides.rs
#![cfg(test)]

use super::super::{load_manifest, remi_manifest_path};
use crate::config::manifest::{Assertion, JsonExpectation};
use crate::engine::{assertions::evaluate_assertion, variables::expand_assertion};
use serde_json::{Value, json};

const PACKAGE: &str = "phase4-daily-driver-corpus";
const LANES: &[&str] = &["fedora44", "ubuntu-26.04", "arch"];

#[test]
fn native_corpus_whatprovides_uses_exact_typed_json_for_all_lanes() {
    let manifest = load_manifest(&remi_manifest_path(
        "phase4-native-daily-driver-corpus.toml",
    ))
    .expect("load native daily-driver corpus manifest");
    let test = manifest
        .test
        .iter()
        .find(|test| test.id == "TNPM16")
        .expect("TNPM16 must own daily-driver queries");
    assert_eq!(test.step.len(), 7);
    let package = &test.step[4];
    let virtual_package = &test.step[5];
    assert_eq!(
        package.conary.as_deref(),
        Some("query whatprovides phase4-daily-driver-corpus --json")
    );
    assert_eq!(
        virtual_package.conary.as_deref(),
        Some("query whatprovides 'virtual(phase4-corpus-tool)' --json")
    );

    for &distro in LANES {
        let vars = manifest
            .distro_overrides
            .get(distro)
            .unwrap_or_else(|| panic!("missing {distro} manifest overrides"));
        let package_assertion = expand_assertion(package.assert.as_ref().unwrap(), vars);
        let virtual_assertion = expand_assertion(virtual_package.assert.as_ref().unwrap(), vars);
        let package_expected = expected_result(
            PACKAGE,
            vars,
            &["1.0", value(vars, "native_corpus_fixture_version")],
        );
        let virtual_expected = expected_result("virtual(phase4-corpus-tool)", vars, &[]);
        assert_typed_result(&package_assertion, &package_expected);
        assert_typed_result(&virtual_assertion, &virtual_expected);
        reject_bad_results(&package_assertion, &package_expected);
        reject_bad_results(&virtual_assertion, &virtual_expected);
        demonstrate_legacy_substring_false_positive(&package_assertion, &package_expected);
    }
}

fn assert_typed_result(assertion: &Assertion, expected: &Value) {
    assert_eq!(assertion.exit_code, Some(0));
    assert!(assertion.exit_code_not.is_none());
    assert!(assertion.stdout_contains.is_none());
    assert!(assertion.stdout_contains_all.is_none());
    assert!(assertion.stdout_contains_any.is_none());
    assert!(assertion.stdout_not_contains.is_none());
    assert!(assertion.stdout_contains_if_success.is_none());
    assert!(assertion.stdout_contains_any_if_success.is_none());
    assert!(assertion.stderr_contains.is_none());
    assert!(assertion.stderr_not_contains.is_none());
    let checks = assertion
        .stdout_json
        .as_ref()
        .expect("typed JSON assertion");
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].pointer, "");
    assert_eq!(
        checks[0].expected,
        JsonExpectation::Equals(expected.clone())
    );
    assert!(evaluate_assertion(assertion, 0, &expected.to_string(), "").is_ok());
}

fn expected_result(
    capability: &str,
    vars: &std::collections::HashMap<String, String>,
    versions: &[&str],
) -> Value {
    json!({
        "schema_version": 1,
        "capability": capability,
        "providers": [{
            "source_kind": "installed",
            "package": {
                "name": PACKAGE,
                "version": value(vars, "native_corpus_fixture_version"),
                "release": "1",
                "architecture": value(vars, "native_arch"),
                "version_scheme": value(vars, "native_scheme"),
            },
            "repository": null,
            "capability_versions": versions,
        }],
        "provider_count": 1,
    })
}

fn value<'a>(vars: &'a std::collections::HashMap<String, String>, name: &str) -> &'a str {
    vars.get(name)
        .unwrap_or_else(|| panic!("missing manifest variable {name}"))
}

fn reject_bad_results(assertion: &Assertion, expected: &Value) {
    let valid = expected.to_string();
    let mut wrong = expected.clone();
    wrong["providers"][0]["package"]["architecture"] = json!("wrong-architecture");
    rejects(assertion, &wrong, "wrong architecture");

    let mut extra_provider = expected.clone();
    extra_provider["providers"]
        .as_array_mut()
        .unwrap()
        .push(expected["providers"][0].clone());
    extra_provider["provider_count"] = json!(2);
    rejects(assertion, &extra_provider, "extra provider");

    for (path, value, defect) in [
        (&["schema_version"][..], json!(2), "wrong schema version"),
        (&["provider_count"][..], json!(0), "zero provider count"),
        (&["provider_count"][..], json!(2), "wrong provider count"),
        (&["provider_count"][..], json!("1"), "string provider count"),
        (&["schema_version"][..], json!("1"), "schema version type"),
        (
            &["capability"][..],
            json!("wrong-capability"),
            "wrong capability",
        ),
        (
            &["providers", "0", "source_kind"][..],
            json!("repository"),
            "wrong source",
        ),
        (
            &["providers", "0", "repository"][..],
            json!({"name":"unexpected"}),
            "unexpected repository",
        ),
        (
            &["providers", "0", "package", "version"][..],
            json!("wrong-version"),
            "wrong package version",
        ),
        (
            &["providers", "0", "package", "version"][..],
            json!(1),
            "package version type",
        ),
        (
            &["providers", "0", "package", "release"][..],
            json!("2"),
            "wrong package release",
        ),
        (
            &["providers", "0", "package", "release"][..],
            Value::Null,
            "null package release",
        ),
        (
            &["providers", "0", "package", "architecture"][..],
            Value::Null,
            "null architecture",
        ),
        (
            &["providers", "0", "capability_versions"][..],
            json!("1.0"),
            "versions array type",
        ),
    ] {
        let mut changed = expected.clone();
        let pointer = format!("/{}", path.join("/"));
        *changed.pointer_mut(&pointer).expect("mutation pointer") = value;
        rejects(assertion, &changed, defect);
    }

    let mut wrong_array = expected.clone();
    wrong_array["providers"] = json!({});
    rejects(assertion, &wrong_array, "providers array type");
    let mut missing_provider = expected.clone();
    missing_provider["providers"] = json!([]);
    missing_provider["provider_count"] = json!(0);
    rejects(assertion, &missing_provider, "missing provider");
    let mut wrong_version = expected.clone();
    let versions = wrong_version["providers"][0]["capability_versions"]
        .as_array_mut()
        .unwrap();
    if !versions.is_empty() {
        versions[0] = json!("wrong-version");
        rejects(assertion, &wrong_version, "wrong capability version");

        let mut missing_version = expected.clone();
        missing_version["providers"][0]["capability_versions"]
            .as_array_mut()
            .unwrap()
            .pop();
        rejects(assertion, &missing_version, "missing capability version");
        let mut duplicate_version = expected.clone();
        duplicate_version["providers"][0]["capability_versions"]
            .as_array_mut()
            .unwrap()
            .push(json!("1.0"));
        rejects(
            assertion,
            &duplicate_version,
            "duplicate capability version",
        );
        let mut reordered_versions = expected.clone();
        reordered_versions["providers"][0]["capability_versions"]
            .as_array_mut()
            .unwrap()
            .swap(0, 1);
        rejects(
            assertion,
            &reordered_versions,
            "reordered capability versions",
        );
    }
    let mut missing_key = expected.clone();
    missing_key
        .as_object_mut()
        .unwrap()
        .remove("provider_count");
    rejects(assertion, &missing_key, "missing root key");
    let mut extra_key = expected.clone();
    extra_key["unexpected"] = json!(true);
    rejects(assertion, &extra_key, "extra root key");
    let mut missing_provider_key = expected.clone();
    missing_provider_key["providers"][0]
        .as_object_mut()
        .unwrap()
        .remove("repository");
    rejects(assertion, &missing_provider_key, "missing provider key");
    let mut extra_package_key = expected.clone();
    extra_package_key["providers"][0]["package"]["unexpected"] = json!(true);
    rejects(assertion, &extra_package_key, "extra package key");

    rejects_stdout(assertion, "not JSON", "malformed JSON");
    rejects_stdout(
        assertion,
        &format!("{valid}{valid}"),
        "concatenated JSON documents",
    );
    assert!(evaluate_assertion(assertion, 1, &valid, "").is_err());
}

fn demonstrate_legacy_substring_false_positive(assertion: &Assertion, expected: &Value) {
    let version = expected["providers"][0]["package"]["version"]
        .as_str()
        .unwrap();
    let old = Assertion {
        exit_code: Some(0),
        stdout_contains_all: Some(vec![
            format!("Capability '{PACKAGE}' is provided by:"),
            format!("{PACKAGE} {version}"),
            "provides version: 1.0".into(),
            format!("provides version: {version}"),
            "Total: 1 provider(s)".into(),
        ]),
        ..Assertion::default()
    };
    let text = format!(
        "Capability '{PACKAGE}' is provided by:\n  {PACKAGE} {version} (provides version: 1.0) (provides version: {version}) [wrong-architecture]\nTotal: 1 provider(s)"
    );
    assert!(evaluate_assertion(&old, 0, &text, "").is_ok());
    let mut wrong_architecture = expected.clone();
    wrong_architecture["providers"][0]["package"]["architecture"] = json!("wrong-architecture");
    rejects(
        assertion,
        &wrong_architecture,
        "wrong architecture accepted by legacy substrings",
    );
}

fn rejects(assertion: &Assertion, value: &Value, defect: &str) {
    assert!(
        evaluate_assertion(assertion, 0, &value.to_string(), "").is_err(),
        "must reject {defect}"
    );
}

fn rejects_stdout(assertion: &Assertion, stdout: &str, defect: &str) {
    assert!(
        evaluate_assertion(assertion, 0, stdout, "").is_err(),
        "must reject {defect}"
    );
}
