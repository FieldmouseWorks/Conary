// apps/conary-test/src/config/tests/native_corpus/typed_update_effect.rs
#![cfg(test)]

use super::super::{load_manifest, remi_manifest_path};
use crate::config::manifest::{Assertion, JsonExpectation};
use crate::engine::{assertions::evaluate_assertion, variables::expand_assertion};
use serde_json::json;

const PACKAGE: &str = "phase4-runtime-fixture";
const UPDATE_COMMAND: &str = "CONARY_TEST_SKIP_GENERATION_MOUNT=1 ${CONARY_BIN} update phase4-runtime-fixture --db-path ${DB_PATH} --root /conary --yes --sandbox always";
const SELECTED_GENERATION_COMMAND: &str = concat!(
    "/opt/remi-tests/fixtures/native/assert-selected-generation.py --root /conary",
    " --expect-sha256 /etc/phase4-runtime-fixture/app.conf=2c76059e3429b1a8429d7026acabffefe3bc738c7344b3f345c2e4d502b1e8dc",
    " --expect-sha256 /usr/bin/phase4-runtime-fixture=67366d905b9464bec8c4ff41021bd536292488fc1777cbf6a1fb41a438b2a283",
);
const UPDATED_ROW_QUERY: &str = "sqlite3 -json ${DB_PATH} \"SELECT name, version, COALESCE(architecture, '') AS architecture, COALESCE(version_scheme, '') AS version_scheme, COALESCE(source_profile, '') AS source_profile FROM troves WHERE name = 'phase4-runtime-fixture'\"";

#[test]
fn native_parity_update_effect_requires_exit_selected_payload_and_exact_row() {
    let manifest = load_manifest(&remi_manifest_path("phase4-native-pm-parity.toml"))
        .expect("load native parity manifest");
    let update = manifest
        .test
        .iter()
        .find(|test| test.id == "TNPM07")
        .expect("TNPM07 must own the local native update");
    assert_eq!(update.step.len(), 7, "TNPM07 must retain seven steps");

    let command = &update.step[3];
    assert_eq!(command.run.as_deref(), Some(UPDATE_COMMAND));
    assert!(command.conary.is_none());
    let command_assertion = command.assert.as_ref().expect("update needs an assertion");
    assert_exit_only(command_assertion);

    let selected = &update.step[4];
    assert_eq!(selected.run.as_deref(), Some(SELECTED_GENERATION_COMMAND));
    assert_exit_only(
        selected
            .assert
            .as_ref()
            .expect("selected payload needs an assertion"),
    );

    let installed = &update.step[5];
    assert_eq!(installed.run.as_deref(), Some(UPDATED_ROW_QUERY));
    let installed_assertion = installed
        .assert
        .as_ref()
        .expect("installed row needs an assertion");
    assert_eq!(installed_assertion.exit_code, Some(0));
    assert!(installed_assertion.exit_code_not.is_none());
    assert_no_output_text_matchers(installed_assertion);

    for (distro, architecture, scheme, profile) in [
        ("fedora44", "x86_64", "rpm", "fedora-44"),
        ("ubuntu-26.04", "amd64", "debian", "ubuntu-26.04"),
        ("arch", "x86_64", "arch", "arch"),
    ] {
        let vars = manifest
            .distro_overrides
            .get(distro)
            .unwrap_or_else(|| panic!("missing {distro} overrides"));
        for (key, expected) in [
            ("native_update_version", "1.0.1-1"),
            ("native_arch", architecture),
            ("native_scheme", scheme),
            ("native_profile", profile),
        ] {
            assert_eq!(
                vars.get(key).map(String::as_str),
                Some(expected),
                "{distro} {key}"
            );
        }

        let expanded = expand_assertion(installed_assertion, vars);
        let checks = expanded.stdout_json.as_ref().expect("typed row assertion");
        assert_eq!(checks.len(), 1, "{distro} must compare one whole document");
        assert_eq!(checks[0].pointer, "", "{distro} must compare the root");
        let expected = json!([{
            "name": PACKAGE,
            "version": "1.0.1-1",
            "architecture": architecture,
            "version_scheme": scheme,
            "source_profile": profile,
        }]);
        assert_eq!(
            checks[0].expected,
            JsonExpectation::Equals(expected.clone()),
            "{distro} must require the exact updated identity"
        );
        assert!(
            evaluate_assertion(&expanded, 0, &expected.to_string(), "").is_ok(),
            "{distro} must accept the exact updated row"
        );
    }
}

#[test]
fn native_parity_update_effect_rejects_nonzero_exit_despite_package_text() {
    let manifest = load_manifest(&remi_manifest_path("phase4-native-pm-parity.toml"))
        .expect("load native parity manifest");
    let update = manifest
        .test
        .iter()
        .find(|test| test.id == "TNPM07")
        .expect("TNPM07 must own the local native update");
    assert_eq!(update.step.len(), 7);
    let assertion = update.step[3].assert.as_ref().expect("update assertion");
    let legacy = Assertion {
        stdout_contains: Some(PACKAGE.to_owned()),
        ..assertion.clone()
    };
    let failure = "Failed to update phase4-runtime-fixture: selected generation unavailable";
    assert!(
        evaluate_assertion(&legacy, 0, failure, "").is_ok(),
        "the old package-name matcher accepted a zero-exit failure sentence"
    );
    assert!(
        evaluate_assertion(assertion, 1, "Updated phase4-runtime-fixture", "").is_err(),
        "the loaded update assertion must reject a nonzero exit despite package text"
    );
    // Exit status alone cannot detect false success; steps 4 and 5 check payload and identity.
    assert!(evaluate_assertion(assertion, 0, failure, "").is_ok());
}

fn assert_exit_only(assertion: &Assertion) {
    assert_eq!(assertion.exit_code, Some(0));
    assert!(assertion.exit_code_not.is_none());
    assert_no_output_text_matchers(assertion);
    assert!(assertion.stdout_json.is_none());
    assert!(assertion.file_exists.is_none());
    assert!(assertion.file_not_exists.is_none());
    assert!(assertion.file_checksum.is_none());
}

fn assert_no_output_text_matchers(assertion: &Assertion) {
    assert!(assertion.stdout_contains.is_none());
    assert!(assertion.stdout_not_contains.is_none());
    assert!(assertion.stdout_contains_all.is_none());
    assert!(assertion.stdout_contains_any.is_none());
    assert!(assertion.stdout_contains_if_success.is_none());
    assert!(assertion.stdout_contains_any_if_success.is_none());
    assert!(assertion.stderr_contains.is_none());
    assert!(assertion.stderr_not_contains.is_none());
}
