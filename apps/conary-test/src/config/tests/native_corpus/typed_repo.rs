// apps/conary-test/src/config/tests/native_corpus/typed_repo.rs
#![cfg(test)]

use super::super::{conary_fixture_path, load_manifest, remi_manifest_path};
use crate::config::load_global_config;
use crate::config::manifest::{Assertion, JsonExpectation, TestStep};
use crate::engine::{
    assertions::evaluate_assertion,
    variables::{build_manifest_variables, expand_assertion},
};
use conary_core::ccs::manifest::CcsManifest;
use serde_json::{Value, json};

const PACKAGE: &str = "phase4-repository-fixture";

const LANES: &[(&str, &str, &str, &str, &str)] = &[
    ("fedora44", "rpm", "remi-fedora-44", "fedora-44", "rpm"),
    (
        "ubuntu-26.04",
        "deb",
        "remi-ubuntu-26.04",
        "ubuntu-26.04",
        "debian",
    ),
    ("arch", "arch", "remi-arch", "arch", "arch"),
];

#[test]
fn native_parity_repo_provenance_requires_exact_typed_rows_for_all_lanes() {
    let path = remi_manifest_path("phase4-native-pm-parity.toml");
    let manifest = load_manifest(&path).expect("load native parity manifest");
    let config = load_global_config(&remi_manifest_path("../config.toml"))
        .expect("load integration distro config");
    let test = manifest
        .test
        .iter()
        .find(|test| test.id == "TNPM10")
        .expect("TNPM10 must own repository install provenance");

    let count = &test.step[2];
    let provenance = &test.step[3];
    assert!(test.step[1].run.as_deref().is_some_and(|run| {
        run.contains("assert-selected-generation.py")
            && run.contains("--present ${repo_install_path}")
    }));
    assert!(
        test.step[4]
            .conary
            .as_deref()
            .is_some_and(|command| command == "list ${repo_install_pkg} --info")
    );
    assert_count_query(count);
    assert_provenance_query(provenance);

    for &(distro_name, target, repository, profile, scheme) in LANES {
        let distro_config = config
            .distros
            .get(distro_name)
            .unwrap_or_else(|| panic!("missing {distro_name} integration config"));
        assert_eq!(
            distro_config.repo_name, repository,
            "{distro_name} repo config"
        );
        assert_eq!(
            distro_config.remi_distro, profile,
            "{distro_name} source profile"
        );

        let overrides = manifest
            .distro_overrides
            .get(distro_name)
            .unwrap_or_else(|| panic!("missing {distro_name} manifest overrides"));
        assert_eq!(
            overrides.get("repo_install_pkg").map(String::as_str),
            Some(PACKAGE)
        );
        assert_eq!(
            overrides.get("native_profile").map(String::as_str),
            Some(profile)
        );
        assert_eq!(
            overrides.get("native_scheme").map(String::as_str),
            Some(scheme)
        );

        let fixture_path =
            conary_fixture_path(&format!("phase4-pinned-repository/{}/ccs.toml", target));
        let fixture = CcsManifest::from_file(&fixture_path)
            .unwrap_or_else(|error| panic!("load {}: {error}", fixture_path.display()));
        assert_eq!(
            fixture.package.name, PACKAGE,
            "{} package fixture",
            distro_name
        );
        assert_eq!(
            fixture.package.version_scheme.as_str(),
            scheme,
            "{} package scheme fixture",
            distro_name
        );

        let vars = build_manifest_variables(&config, distro_name, &manifest);
        let count_assertion = expand_assertion(count.assert.as_ref().unwrap(), &vars);
        let provenance_assertion = expand_assertion(provenance.assert.as_ref().unwrap(), &vars);
        let expected_row = json!([{
            "name": PACKAGE,
            "repository_name": repository,
            "source_profile": profile,
            "version_scheme": scheme,
            "install_source": "repository",
            "install_reason": "explicit",
        }]);
        assert!(evaluate_assertion(&count_assertion, 0, r#"[{"repo_troves":1}]"#, "").is_ok());
        assert!(
            evaluate_assertion(&provenance_assertion, 0, &expected_row.to_string(), "").is_ok()
        );
        reject_bad_count_results(&count_assertion);
        reject_bad_provenance_results(&provenance_assertion, &expected_row);
        reject_malformed_output(&count_assertion, r#"[{"repo_troves":1}]"#);
        reject_malformed_output(&provenance_assertion, &expected_row.to_string());
        demonstrate_legacy_substring_false_positives(repository, profile, scheme);
    }
}

fn assert_count_query(step: &TestStep) {
    let sql = sqlite_query(step);
    let (projection, source) = sql.split_once(" FROM ").expect("count query source");
    assert_eq!(projection, "SELECT COUNT(*) AS repo_troves");
    assert_eq!(source, "troves WHERE name = '${repo_install_pkg}'");
    assert_eq!(sql.matches("SELECT").count(), 1);
    assert!(!sql.contains(';'));
    assert_typed_root_assertion(step);
}

fn assert_provenance_query(step: &TestStep) {
    let sql = sqlite_query(step);
    let (select, from) = sql.split_once(" FROM troves t LEFT JOIN repositories r ON r.id = t.installed_from_repository_id WHERE t.name = '${repo_install_pkg}'")
        .expect("provenance query must keep its exact join and package predicate");
    assert_eq!(
        select
            .strip_prefix("SELECT ")
            .unwrap()
            .split(", ")
            .collect::<Vec<_>>(),
        [
            "t.name AS name",
            "COALESCE(r.name,'') AS repository_name",
            "COALESCE(t.source_profile,'') AS source_profile",
            "COALESCE(t.version_scheme,'') AS version_scheme",
            "COALESCE(t.install_source,'') AS install_source",
            "COALESCE(t.install_reason,'') AS install_reason",
        ]
    );
    assert!(from.is_empty());
    assert_eq!(sql.matches("SELECT").count(), 1);
    assert!(!sql.contains(';'));
    assert_typed_root_assertion(step);
}

fn sqlite_query(step: &TestStep) -> &str {
    let command = step
        .run
        .as_deref()
        .expect("repository check must run sqlite3");
    assert!(command.starts_with("sqlite3 -json ${DB_PATH} \""));
    command
        .strip_prefix("sqlite3 -json ${DB_PATH} \"")
        .and_then(|sql| sql.strip_suffix('"'))
        .expect("sqlite query must be one quoted command")
}

fn assert_typed_root_assertion(step: &TestStep) {
    let assertion = step.assert.as_ref().expect("repository query assertion");
    assert_eq!(assertion.exit_code, Some(0));
    assert!(assertion.exit_code_not.is_none());
    assert!(assertion.stdout_contains.is_none());
    assert!(assertion.stdout_contains_all.is_none());
    let checks = assertion
        .stdout_json
        .as_ref()
        .expect("exact JSON assertion");
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].pointer, "");
    assert!(matches!(checks[0].expected, JsonExpectation::Equals(_)));
}

fn reject_bad_count_results(assertion: &Assertion) {
    for (stdout, defect) in [
        (r#"[{"repo_troves":0}]"#, "zero count"),
        (r#"[{"repo_troves":11}]"#, "eleven count"),
        (r#"[{"repo_troves":"1"}]"#, "string count"),
        (r#"[{"repo_troves":1.0}]"#, "float count"),
        ("[]", "missing row"),
        (r#"[{"repo_troves":1},{"repo_troves":1}]"#, "extra row"),
        (r#"[{"repo_troves":1,"extra":true}]"#, "extra key"),
    ] {
        rejects(assertion, stdout, defect);
    }
    let missing_key = r#"[{}]"#;
    rejects(assertion, missing_key, "missing count key");
}

fn reject_bad_provenance_results(assertion: &Assertion, expected: &Value) {
    for (field, wrong) in [
        ("name", json!("wrong-package")),
        ("repository_name", json!("wrong-repository")),
        ("source_profile", json!("wrong-profile")),
        ("version_scheme", json!("wrong-scheme")),
        ("install_source", json!("file")),
        ("install_reason", json!("dependency")),
    ] {
        let mut changed = expected.clone();
        changed[0][field] = wrong;
        rejects(assertion, &changed.to_string(), field);

        let mut null_value = expected.clone();
        null_value[0][field] = Value::Null;
        rejects(assertion, &null_value.to_string(), &format!("null {field}"));
    }
    let mut missing_row = expected.clone();
    missing_row.as_array_mut().unwrap().clear();
    rejects(assertion, &missing_row.to_string(), "missing row");
    let mut extra_row = expected.clone();
    extra_row.as_array_mut().unwrap().push(expected[0].clone());
    rejects(assertion, &extra_row.to_string(), "extra row");
    let mut missing_key = expected.clone();
    missing_key[0]
        .as_object_mut()
        .unwrap()
        .remove("install_reason");
    rejects(assertion, &missing_key.to_string(), "missing key");
    let mut extra_key = expected.clone();
    extra_key[0]
        .as_object_mut()
        .unwrap()
        .insert("unexpected".into(), json!(true));
    rejects(assertion, &extra_key.to_string(), "extra key");
}

fn reject_malformed_output(assertion: &Assertion, valid_stdout: &str) {
    rejects(assertion, "not JSON", "malformed JSON");
    rejects(
        assertion,
        &format!("{valid_stdout}{valid_stdout}"),
        "concatenated valid JSON documents",
    );
    assert!(
        evaluate_assertion(assertion, 1, valid_stdout, "").is_err(),
        "must reject a nonzero command exit with valid JSON"
    );
}

fn rejects(assertion: &Assertion, stdout: &str, defect: &str) {
    assert!(
        evaluate_assertion(assertion, 0, stdout, "").is_err(),
        "must reject {defect}"
    );
}

fn demonstrate_legacy_substring_false_positives(repository: &str, profile: &str, scheme: &str) {
    let old_count = Assertion {
        stdout_contains: Some("1 repo troves".into()),
        ..Assertion::default()
    };
    assert!(evaluate_assertion(&old_count, 0, "11 repo troves", "").is_ok());

    let old_provenance = Assertion {
        stdout_contains_all: Some(vec![format!(
            "{PACKAGE}|{}|{}|{}|repository|explicit",
            repository, profile, scheme
        )]),
        ..Assertion::default()
    };
    let suffixed_reason = format!(
        "{PACKAGE}|{}|{}|{}|repository|explicit-suffix",
        repository, profile, scheme
    );
    assert!(evaluate_assertion(&old_provenance, 0, &suffixed_reason, "").is_ok());
}
