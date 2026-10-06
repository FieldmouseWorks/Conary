// apps/conary-test/src/config/tests/native_corpus/typed_autoremove.rs

use super::super::{load_manifest, remi_manifest_path};
use crate::config::manifest::Assertion;
use crate::engine::{
    assertions::evaluate_assertion,
    variables::{expand_assertion, expand_variables},
};
use serde_json::{Value, json};

#[test]
fn native_parity_autoremove_preview_requires_the_exact_typed_plan() {
    let path = remi_manifest_path("phase4-native-pm-parity.toml");
    let manifest = load_manifest(&path).expect("load native parity manifest");
    let autoremove = manifest
        .test
        .iter()
        .find(|test| test.id == "TNPM12")
        .expect("TNPM12 must own the autoremove contract");
    let preview = autoremove
        .step
        .iter()
        .find(|step| {
            step.run.as_deref().is_some_and(|command| {
                command.contains("autoremove") && command.contains("--dry-run")
            })
        })
        .expect("TNPM12 must run an autoremove preview");
    assert!(
        preview.run.as_deref().unwrap().contains("--json"),
        "autoremove preview must emit typed JSON"
    );
    let assertion = preview
        .assert
        .as_ref()
        .expect("autoremove preview must assert its plan");
    assert!(assertion.stdout_contains_all.is_none());
    assert!(assertion.stdout_json.is_some());

    for distro in ["fedora44", "ubuntu-26.04", "arch"] {
        let vars = manifest
            .distro_overrides
            .get(distro)
            .unwrap_or_else(|| panic!("missing {distro} overrides"));
        let assertion = expand_assertion(assertion, vars);
        let package = vars
            .get("repo_install_pkg")
            .expect("repository package name must be pinned");
        let architecture = vars
            .get("native_arch")
            .expect("native architecture must be pinned");
        let plan = json!({
            "operation": "package.autoremove.plan",
            "status": "planned",
            "data": {
                "schema_version": 2,
                "removable": [{
                    "name": package,
                    "version": "1.0.0",
                    "package_release": "1",
                    "architecture": architecture,
                    "round": 1,
                }],
            },
        });
        assert!(
            evaluate_assertion(&assertion, 0, &plan.to_string(), "").is_ok(),
            "{distro} must accept the pinned one-package plan"
        );

        for (field, wrong_value) in [
            ("operation", json!("package.install.plan")),
            ("status", json!("ok")),
        ] {
            let mut wrong = plan.clone();
            wrong[field] = wrong_value;
            assert_rejected(&assertion, &wrong, distro, field);
        }
        let mut wrong_schema = plan.clone();
        wrong_schema["data"]["schema_version"] = json!(3);
        assert_rejected(&assertion, &wrong_schema, distro, "schema_version");

        let mut wrong_package = plan.clone();
        wrong_package["data"]["removable"][0]["name"] = json!("wrong-package");
        assert_rejected(&assertion, &wrong_package, distro, "package name");

        let mut extra_package = plan.clone();
        extra_package["data"]["removable"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "name": "unexpected-orphan",
                "version": "1.0.0",
                "package_release": "1",
                "architecture": architecture,
                "round": 1,
            }));
        assert_rejected(&assertion, &extra_package, distro, "extra removal");
    }
}

#[test]
fn native_parity_autoremove_final_state_requires_exact_json_zero_count() {
    let path = remi_manifest_path("phase4-native-pm-parity.toml");
    let manifest = load_manifest(&path).expect("load native parity manifest");
    let autoremove = manifest
        .test
        .iter()
        .find(|test| test.id == "TNPM12")
        .expect("TNPM12 must own the autoremove contract");
    let final_count = autoremove
        .step
        .last()
        .expect("TNPM12 must finish with the final package count");
    let command = final_count.run.as_deref().unwrap();
    assert!(command.starts_with("sqlite3 -json ${DB_PATH} "));
    assert!(command.contains("SELECT COUNT(*) AS installed FROM troves WHERE name = "));

    let assertion = final_count
        .assert
        .as_ref()
        .expect("final package count must be asserted");
    assert_eq!(assertion.exit_code, Some(0));
    assert!(assertion.stdout_contains.is_none());
    assert!(assertion.stdout_json.is_some());

    let legacy_substring = Assertion {
        stdout_contains: Some("0 installed".to_owned()),
        ..Assertion::default()
    };
    assert!(evaluate_assertion(&legacy_substring, 0, "10 installed", "").is_ok());

    assert!(evaluate_assertion(assertion, 0, "[{\"installed\":0}]", "").is_ok());
    for (stdout, defect) in [
        ("[{\"installed\":1}]", "count of one"),
        ("[{\"installed\":10}]", "count of ten"),
        ("[{\"installed\":\"0\"}]", "string zero"),
        ("[{\"installed\":0.0}]", "floating-point zero"),
        ("[]", "missing row"),
        ("[{}]", "missing count key"),
        ("[{\"installed\":0},{\"installed\":1}]", "extra row"),
        (
            "[{\"installed\":0,\"unexpected\":true}]",
            "extra object key",
        ),
        ("not JSON", "malformed JSON"),
    ] {
        assert!(
            evaluate_assertion(assertion, 0, stdout, "").is_err(),
            "must reject {defect}"
        );
    }
    assert!(
        evaluate_assertion(assertion, 1, "[{\"installed\":0}]", "").is_err(),
        "must reject a nonzero command exit"
    );

    for distro in ["fedora44", "ubuntu-26.04", "arch"] {
        let vars = manifest
            .distro_overrides
            .get(distro)
            .unwrap_or_else(|| panic!("missing {distro} overrides"));
        let package = vars
            .get("repo_install_pkg")
            .expect("repository package name must be pinned");
        let expanded_command = expand_variables(command, vars);
        assert!(
            expanded_command.contains(&format!(
                "SELECT COUNT(*) AS installed FROM troves WHERE name = '{package}'"
            )),
            "{distro} command must count its configured repository package"
        );
    }
}

fn assert_rejected(
    assertion: &crate::config::manifest::Assertion,
    plan: &Value,
    distro: &str,
    defect: &str,
) {
    assert!(
        evaluate_assertion(assertion, 0, &plan.to_string(), "").is_err(),
        "{distro} must reject a plan with the wrong {defect}"
    );
}
