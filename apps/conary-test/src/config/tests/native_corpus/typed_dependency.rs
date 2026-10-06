// apps/conary-test/src/config/tests/native_corpus/typed_dependency.rs
#![cfg(test)]

use super::super::{load_manifest, remi_manifest_path};
use crate::config::{
    load_global_config,
    manifest::{Assertion, JsonExpectation, TestManifest},
};
use crate::engine::{
    assertions::evaluate_assertion,
    variables::{build_manifest_variables, expand_assertion, expand_variables},
};
use conary_core::repository::versioning::VersionScheme;
use serde_json::{Value, json};

mod fixtures;
use fixtures::{
    execute_json_query, insert_trove, legacy_stdout, production_database, reject_persisted_defects,
};

const NAME: &str = "phase4-repository-fixture";
const QUERY: &str = "SELECT t.name AS name, t.version AS version, COALESCE(t.package_release, '') AS package_release, COALESCE(t.architecture, '') AS architecture, COALESCE(t.version_scheme, '') AS version_scheme, COALESCE(r.name, '') AS repository_name, COALESCE(t.source_profile, '') AS source_profile, COALESCE(t.install_source, '') AS install_source, COALESCE(t.install_reason, '') AS install_reason FROM troves t LEFT JOIN repositories r ON r.id = t.installed_from_repository_id WHERE t.name = 'phase4-repository-fixture' ORDER BY t.id";
const LEGACY_QUERY: &str = "SELECT t.name || '|' || t.version || '|' || COALESCE(t.package_release, '') || '|' || COALESCE(t.architecture, '') || '|' || COALESCE(t.version_scheme, '') || '|' || COALESCE(r.name, '') || '|' || COALESCE(t.source_profile, '') || '|' || COALESCE(t.install_source, '') || '|' || COALESCE(t.install_reason, '') FROM troves t LEFT JOIN repositories r ON r.id = t.installed_from_repository_id WHERE t.name = 'phase4-repository-fixture'";
const COLUMNS: &[&str] = &[
    "name",
    "version",
    "package_release",
    "architecture",
    "version_scheme",
    "repository_name",
    "source_profile",
    "install_source",
    "install_reason",
];

struct Lane {
    distro: &'static str,
    architecture: &'static str,
    scheme: VersionScheme,
    profile: &'static str,
    repository: &'static str,
}

const LANES: &[Lane] = &[
    Lane {
        distro: "fedora44",
        architecture: "x86_64",
        scheme: VersionScheme::Rpm,
        profile: "fedora-44",
        repository: "remi-fedora-44",
    },
    Lane {
        distro: "ubuntu-26.04",
        architecture: "amd64",
        scheme: VersionScheme::Debian,
        profile: "ubuntu-26.04",
        repository: "remi-ubuntu-26.04",
    },
    Lane {
        distro: "arch",
        architecture: "x86_64",
        scheme: VersionScheme::Arch,
        profile: "arch",
        repository: "remi-arch",
    },
];

#[test]
fn native_corpus_tnpm14_dependency_requires_one_exact_repository_row() {
    let manifest = load_manifest(&remi_manifest_path(
        "phase4-native-daily-driver-corpus.toml",
    ))
    .expect("load native daily-driver manifest");
    let config = load_global_config(&remi_manifest_path("../config.toml"))
        .expect("load integration distro config");
    let test = manifest
        .test
        .iter()
        .find(|test| test.id == "TNPM14")
        .expect("TNPM14");
    assert_eq!(
        test.step.len(),
        4,
        "retain trigger, install, dependency, and selected-root checks"
    );
    assert!(
        test.step[0]
            .run
            .as_deref()
            .is_some_and(|run| run.contains("system trigger add phase4-corpus-trigger"))
    );
    assert!(test.step[1].run.as_deref().is_some_and(|run| {
        run.contains("install \"$NATIVE_PKG_FILE\"") && run.contains("--convert-to-ccs")
    }));
    assert!(
        test.step[3]
            .run
            .as_deref()
            .is_some_and(|run| run.contains("assert-selected-generation.py")
                && run.contains("--present /usr/share/phase4-repository-fixture/probe.txt"))
    );
    for step in [&test.step[0], &test.step[1], &test.step[3]] {
        assert_eq!(
            step.assert
                .as_ref()
                .and_then(|assertion| assertion.exit_code),
            Some(0)
        );
    }

    let step = &test.step[2];
    let command = step.run.as_deref().expect("dependency SQL command");
    assert_eq!(command, format!("sqlite3 -json ${{DB_PATH}} \"{QUERY}\""));
    assert_eq!(command.matches("sqlite3").count(), 1);
    assert_eq!(QUERY.matches("SELECT ").count(), 1);
    assert!(!QUERY.contains(';'));
    let assertion = step.assert.as_ref().expect("dependency assertion");
    assert_typed_root(assertion);

    for lane in LANES {
        let overrides = &manifest.distro_overrides[lane.distro];
        let distro = &config.distros[lane.distro];
        assert_eq!(distro.repo_name, lane.repository);
        assert_eq!(distro.remi_distro, lane.profile);
        assert_eq!(overrides["native_arch"], lane.architecture);
        assert_eq!(overrides["native_scheme"], lane.scheme.as_str());
        assert_eq!(overrides["native_profile"], lane.profile);
        let variables = build_manifest_variables(&config, lane.distro, &manifest);
        assert_eq!(variables["REPO_NAME"], lane.repository);
        assert_eq!(expand_variables(QUERY, &variables), QUERY);
        let expanded = expand_assertion(assertion, &variables);
        let expected = expected_rows(lane);
        assert_eq!(
            expanded.stdout_json.as_ref().unwrap()[0].expected,
            JsonExpectation::Equals(expected.clone()),
            "{} literal manifest row",
            lane.distro
        );

        let (_directory, database, target_id, repository_id) = production_database(lane);
        let actual = execute_json_query(&database, QUERY);
        assert_eq!(
            actual, expected,
            "{} current-schema SQL projection",
            lane.distro
        );
        assert!(evaluate_assertion(&expanded, 0, &actual.to_string(), "").is_ok());
        reject_persisted_defects(&database, target_id, &expanded);
        reject_stdout_defects(&expanded, &expected);

        insert_trove(&database, lane, NAME, "0.9.0", Some(repository_id));
        let duplicate = execute_json_query(&database, QUERY);
        assert_eq!(duplicate.as_array().unwrap().len(), 2);
        assert_eq!(
            duplicate[0], expected[0],
            "ORDER BY t.id keeps the original row first"
        );
        assert_eq!(duplicate[1]["version"], "0.9.0");
        reject(
            &expanded,
            &duplicate,
            "extra same-name, different-version row",
        );

        let legacy_stdout = legacy_stdout(&database);
        let legacy = Assertion {
            exit_code: Some(0),
            stdout_contains: Some(legacy_row(lane)),
            ..Assertion::default()
        };
        assert!(
            evaluate_assertion(&legacy, 0, &legacy_stdout, "").is_ok(),
            "old substring assertion accepts the extra same-name row"
        );

        let mut two_row_oracle = expanded.clone();
        two_row_oracle.stdout_json.as_mut().unwrap()[0].expected =
            JsonExpectation::Equals(duplicate.clone());
        assert!(evaluate_assertion(&two_row_oracle, 0, &duplicate.to_string(), "").is_ok());
        let mut reversed = duplicate;
        reversed.as_array_mut().unwrap().reverse();
        reject(
            &two_row_oracle,
            &reversed,
            "wrong t.id row order at equal count",
        );
    }
}

fn assert_typed_root(assertion: &Assertion) {
    assert_eq!(assertion.exit_code, Some(0));
    assert!(assertion.exit_code_not.is_none());
    assert!(assertion.stdout_contains.is_none());
    assert!(assertion.stdout_not_contains.is_none());
    assert!(assertion.stdout_contains_all.is_none());
    assert!(assertion.stdout_contains_any.is_none());
    assert!(assertion.stdout_contains_if_success.is_none());
    assert!(assertion.stdout_contains_any_if_success.is_none());
    let [root] = assertion.stdout_json.as_deref().expect("one JSON check") else {
        panic!("dependency proof must compare one JSON document");
    };
    assert_eq!(root.pointer, "");
}

pub(super) fn assert_tnpm14_dependency_shape(manifest: &TestManifest) {
    let test = manifest
        .test
        .iter()
        .find(|test| test.id == "TNPM14")
        .expect("TNPM14");
    let assertion = test.step[2].assert.as_ref().expect("dependency assertion");
    assert_typed_root(assertion);
    let JsonExpectation::Equals(rows) = &assertion.stdout_json.as_ref().unwrap()[0].expected else {
        panic!("TNPM14 dependency must assert exact root JSON");
    };
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(rows[0]["repository_name"].as_str(), Some("${REPO_NAME}"));
    assert_eq!(rows[0]["install_source"].as_str(), Some("repository"));
    assert_eq!(rows[0]["install_reason"].as_str(), Some("dependency"));
}

fn expected_rows(lane: &Lane) -> Value {
    json!([{"name":NAME, "version":"1.0.0", "package_release":"1",
        "architecture":lane.architecture, "version_scheme":lane.scheme.as_str(),
        "repository_name":lane.repository, "source_profile":lane.profile,
        "install_source":"repository", "install_reason":"dependency"}])
}

fn reject_stdout_defects(assertion: &Assertion, expected: &Value) {
    for field in COLUMNS {
        for wrong_value in [json!("wrong-value"), json!(7), json!(null)] {
            let mut wrong = expected.clone();
            wrong[0][*field] = wrong_value;
            reject(assertion, &wrong, field);
        }
        let mut missing = expected.clone();
        missing[0].as_object_mut().unwrap().remove(*field);
        reject(assertion, &missing, "missing named field");
    }
    let mut extra_key = expected.clone();
    extra_key[0]["unexpected"] = json!(true);
    reject(assertion, &extra_key, "extra field");
    reject(assertion, &json!([]), "zero rows");
    reject(
        assertion,
        &json!([expected[0].clone(), expected[0].clone()]),
        "duplicate row",
    );
    reject(assertion, &json!({}), "wrong root type");
    let valid = expected.to_string();
    for malformed in [
        "not JSON",
        &format!("{valid} trailing"),
        &format!("{valid}{valid}"),
    ] {
        assert!(
            evaluate_assertion(assertion, 0, malformed, "").is_err(),
            "{malformed}"
        );
    }
    assert!(
        evaluate_assertion(assertion, 1, &valid, "").is_err(),
        "nonzero sqlite3 exit"
    );
}

fn reject(assertion: &Assertion, actual: &Value, defect: &str) {
    assert!(
        evaluate_assertion(assertion, 0, &actual.to_string(), "").is_err(),
        "must reject {defect}"
    );
}

fn legacy_row(lane: &Lane) -> String {
    format!(
        "{NAME}|1.0.0|1|{}|{}|{}|{}|repository|dependency",
        lane.architecture,
        lane.scheme.as_str(),
        lane.repository,
        lane.profile
    )
}
