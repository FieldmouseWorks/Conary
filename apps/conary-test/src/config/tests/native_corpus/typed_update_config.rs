// apps/conary-test/src/config/tests/native_corpus/typed_update_config.rs
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
use conary_core::{db::models::ConfigSource, repository::versioning::VersionScheme};
use serde_json::{Value, json};

mod fixtures;
use fixtures::{
    COLUMNS, assert_legacy_accepts, execute_json_query, insert_config, insert_trove,
    production_database, reject, reject_persisted_defects,
};

const NAME: &str = "phase4-daily-driver-corpus";
const CONFIG_PATH: &str = "/etc/phase4-corpus/app.conf";
const HASH: &str = "97d836d4bf6c4c49fa763836c117d738fef15b7d995bc2cf82e2a02704364d27";
const REPOSITORY: &str = "w7-native-update";
const QUERY: &str = concat!(
    "SELECT t.name AS trove_name, t.version AS trove_version, ",
    "t.architecture AS trove_architecture, t.version_scheme AS trove_version_scheme, ",
    "t.source_profile AS trove_source_profile, r.name AS repository_name, ",
    "t.install_source AS install_source, t.install_reason AS install_reason, ",
    "c.path AS config_path, c.package_name AS config_package_name, ",
    "c.package_version AS config_package_version, ",
    "c.package_architecture AS config_package_architecture, ",
    "c.original_hash AS original_hash, c.current_hash AS current_hash, ",
    "c.noreplace AS noreplace, c.status AS status, c.source AS source, ",
    "CASE WHEN c.trove_id = t.id THEN 1 ELSE 0 END AS config_owner_match, ",
    "(SELECT COUNT(*) FROM config_files x WHERE x.trove_id = t.id OR x.package_name = t.name) AS related_config_rows ",
    "FROM troves t LEFT JOIN repositories r ON r.id = t.installed_from_repository_id ",
    "LEFT JOIN config_files c ON c.path = '/etc/phase4-corpus/app.conf' ",
    "WHERE t.name = 'phase4-daily-driver-corpus' ORDER BY t.id"
);
struct Lane {
    distro: &'static str,
    architecture: &'static str,
    scheme: VersionScheme,
    profile: &'static str,
    source: ConfigSource,
}

const LANES: &[Lane] = &[
    Lane {
        distro: "fedora44",
        architecture: "x86_64",
        scheme: VersionScheme::Rpm,
        profile: "fedora-44",
        source: ConfigSource::Rpm,
    },
    Lane {
        distro: "ubuntu-26.04",
        architecture: "amd64",
        scheme: VersionScheme::Debian,
        profile: "ubuntu-26.04",
        source: ConfigSource::Deb,
    },
    Lane {
        distro: "arch",
        architecture: "x86_64",
        scheme: VersionScheme::Arch,
        profile: "arch",
        source: ConfigSource::Arch,
    },
];

#[test]
fn native_corpus_tnpm18_update_config_requires_exact_current_schema_row() {
    let manifest = load_manifest(&remi_manifest_path(
        "phase4-native-daily-driver-corpus.toml",
    ))
    .expect("load daily-driver manifest");
    let config = load_global_config(&remi_manifest_path("../config.toml"))
        .expect("load integration distro config");
    assert_tnpm18_update_config_shape(&manifest);
    let assertion = manifest
        .test
        .iter()
        .find(|test| test.id == "TNPM18")
        .unwrap()
        .step[7]
        .assert
        .as_ref()
        .unwrap();

    for lane in LANES {
        let overrides = &manifest.distro_overrides[lane.distro];
        assert_eq!(overrides["native_arch"], lane.architecture);
        assert_eq!(overrides["native_scheme"], lane.scheme.as_str());
        assert_eq!(overrides["native_profile"], lane.profile);
        assert_eq!(
            overrides["native_corpus_config_source"],
            lane.source.as_str()
        );
        assert_eq!(overrides["native_corpus_update_version"], "1.0.1-1");
        let variables = build_manifest_variables(&config, lane.distro, &manifest);
        assert_eq!(expand_variables(QUERY, &variables), QUERY);
        let expanded = expand_assertion(assertion, &variables);
        let expected = expected_rows(lane);
        assert_eq!(
            expanded.stdout_json.as_ref().unwrap()[0].expected,
            JsonExpectation::Equals(expected.clone()),
            "{} literal manifest row",
            lane.distro
        );

        let (_directory, database, target_id, other_id, repository_id) = production_database(lane);
        let actual = execute_json_query(&database, QUERY);
        assert_eq!(
            actual, expected,
            "{} current-schema SQL projection",
            lane.distro
        );
        assert!(evaluate_assertion(&expanded, 0, &actual.to_string(), "").is_ok());
        reject_stdout_defects(&expanded, &expected);
        reject_persisted_defects(&database, target_id, other_id, &expanded);

        database.execute_batch("SAVEPOINT extra_trove").unwrap();
        insert_trove(&database, lane, NAME, "0.9.0", repository_id);
        let duplicate = execute_json_query(&database, QUERY);
        assert_eq!(duplicate.as_array().unwrap().len(), 2);
        assert_eq!(
            duplicate[0], expected[0],
            "ORDER BY t.id keeps original first"
        );
        assert_eq!(duplicate[1]["trove_version"], "0.9.0");
        assert_legacy_accepts(&database, lane, &expanded, &duplicate);
        database
            .execute_batch("ROLLBACK TO extra_trove; RELEASE extra_trove")
            .unwrap();

        database.execute_batch("SAVEPOINT extra_config").unwrap();
        insert_config(
            &database,
            target_id,
            lane.source,
            "/etc/phase4-corpus/unexpected.conf",
        );
        database
            .execute(
                "UPDATE config_files SET trove_id = NULL WHERE path = ?1",
                ["/etc/phase4-corpus/unexpected.conf"],
            )
            .unwrap();
        let overfull = execute_json_query(&database, QUERY);
        assert_eq!(overfull[0]["related_config_rows"], 5);
        assert_legacy_accepts(&database, lane, &expanded, &overfull);
        database
            .execute_batch("ROLLBACK TO extra_config; RELEASE extra_config")
            .unwrap();
    }
}

pub(super) fn assert_tnpm18_update_config_shape(manifest: &TestManifest) {
    let test = manifest
        .test
        .iter()
        .find(|test| test.id == "TNPM18")
        .expect("TNPM18");
    assert_eq!(
        test.step.len(),
        10,
        "retain update, selected-root, config, and lifecycle steps"
    );
    assert!(
        test.step[3]
            .run
            .as_deref()
            .is_some_and(|run| run.contains(" update "))
    );
    assert!(
        test.step[4]
            .run
            .as_deref()
            .is_some_and(|run| run.contains("assert-selected-generation.py"))
    );
    assert!(
        test.step[6]
            .run
            .as_deref()
            .is_some_and(|run| run.contains("remove_on_upgrade"))
    );
    assert!(
        test.step[8]
            .run
            .as_deref()
            .is_some_and(|run| run.contains("installed_native_lifecycle_bundles"))
    );
    let step = &test.step[7];
    assert_eq!(
        step.run.as_deref(),
        Some(format!("sqlite3 -json ${{DB_PATH}} \"{QUERY}\"").as_str())
    );
    assert_eq!(step.run.as_ref().unwrap().matches("sqlite3").count(), 1);
    assert!(!QUERY.contains(';'));
    assert!(QUERY.ends_with("ORDER BY t.id"));
    let assertion = step
        .assert
        .as_ref()
        .expect("TNPM18 update config assertion");
    assert_typed_root(assertion);
    let JsonExpectation::Equals(rows) = &assertion.stdout_json.as_ref().unwrap()[0].expected else {
        panic!("TNPM18 must assert exact root JSON");
    };
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(rows[0]["repository_name"], "w7-native-update");
    assert_eq!(rows[0]["config_owner_match"], 1);
    assert_eq!(rows[0]["related_config_rows"], 4);
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
        panic!("TNPM18 must compare one JSON document");
    };
    assert_eq!(root.pointer, "");
}

fn expected_rows(lane: &Lane) -> Value {
    json!([{"trove_name":NAME, "trove_version":"1.0.1-1",
        "trove_architecture":lane.architecture, "trove_version_scheme":lane.scheme.as_str(),
        "trove_source_profile":lane.profile, "repository_name":REPOSITORY,
        "install_source":"repository", "install_reason":"explicit",
        "config_path":CONFIG_PATH, "config_package_name":NAME,
        "config_package_version":"1.0.1-1", "config_package_architecture":lane.architecture,
        "original_hash":HASH, "current_hash":HASH, "noreplace":1,
        "status":"pristine", "source":lane.source.as_str(),
        "config_owner_match":1, "related_config_rows":4}])
}

fn reject_stdout_defects(assertion: &Assertion, expected: &Value) {
    for field in COLUMNS {
        let mut wrong = expected.clone();
        wrong[0][*field] = if expected[0][*field].is_number() {
            json!("1")
        } else {
            json!(7)
        };
        reject(assertion, &wrong, field);
        let mut null = expected.clone();
        null[0][*field] = Value::Null;
        reject(assertion, &null, "null field");
        let mut missing = expected.clone();
        missing[0].as_object_mut().unwrap().remove(*field);
        reject(assertion, &missing, "missing field");
    }
    let mut extra = expected.clone();
    extra[0]["unexpected"] = json!(true);
    reject(assertion, &extra, "extra field");
    reject(assertion, &json!([]), "missing row");
    reject(
        assertion,
        &json!([expected[0].clone(), expected[0].clone()]),
        "extra row",
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
            "must reject {malformed}"
        );
    }
    assert!(
        evaluate_assertion(assertion, 1, &valid, "").is_err(),
        "must reject nonzero sqlite3 exit"
    );
}
