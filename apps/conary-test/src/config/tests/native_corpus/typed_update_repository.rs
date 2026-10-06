// apps/conary-test/src/config/tests/native_corpus/typed_update_repository.rs
#![cfg(test)]

use super::super::{load_manifest, remi_manifest_path};
use crate::config::{
    load_global_config,
    manifest::{Assertion, JsonExpectation, TestManifest},
};
use crate::engine::{
    assertions::evaluate_assertion,
    variables::{build_manifest_variables, expand_assertion},
};
use conary_core::db;
use rusqlite::{Connection, params, types::ValueRef};
use serde_json::{Value, json};

const REPOSITORY: &str = "w7-native-update";
const PACKAGE: &str = "phase4-daily-driver-corpus";
const VERSION: &str = "1.0.1-1";
const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const QUERY: &str = concat!(
    "SELECT r.name AS repository_name, r.default_strategy AS default_strategy, ",
    "r.source_profile AS source_profile, rp.name AS package_name, ",
    "rp.version AS package_version, rp.version_scheme AS version_scheme, ",
    "rp.architecture AS architecture, ",
    "(rp.checksum = '$NATIVE_UPDATE_CCS_SHA256') AS checksum_matches_fixture, ",
    "k.status AS key_status FROM repositories r ",
    "JOIN repository_packages rp ON rp.repository_id = r.id ",
    "JOIN repository_package_keys k ON k.repository_id = r.id ",
    "WHERE r.name = '$NATIVE_UPDATE_REPOSITORY' ORDER BY rp.id, k.public_key"
);
const LEGACY_QUERY: &str = concat!(
    "SELECT r.name || '|' || r.default_strategy || '|' || r.source_profile || '|' || ",
    "rp.name || '|' || rp.version || '|' || rp.version_scheme || '|' || ",
    "rp.architecture || '|' || rp.checksum || '|' || k.status ",
    "FROM repositories r JOIN repository_packages rp ON rp.repository_id = r.id ",
    "JOIN repository_package_keys k ON k.repository_id = r.id ",
    "WHERE r.name = 'w7-native-update'"
);
const COLUMNS: &[&str] = &[
    "repository_name",
    "default_strategy",
    "source_profile",
    "package_name",
    "package_version",
    "version_scheme",
    "architecture",
    "checksum_matches_fixture",
    "key_status",
];

type Lane = (&'static str, &'static str, &'static str, &'static str);
const LANES: &[Lane] = &[
    ("fedora44", "x86_64", "rpm", "fedora-44"),
    ("ubuntu-26.04", "amd64", "debian", "ubuntu-26.04"),
    ("arch", "x86_64", "arch", "arch"),
];

#[test]
fn native_corpus_tnpm18_repository_requires_exact_current_schema_rows() {
    let manifest = load_manifest(&remi_manifest_path(
        "phase4-native-daily-driver-corpus.toml",
    ))
    .expect("load daily-driver manifest");
    let config = load_global_config(&remi_manifest_path("../config.toml"))
        .expect("load integration distro config");
    let assertion = manifest
        .test
        .iter()
        .find(|test| test.id == "TNPM18")
        .unwrap()
        .step[1]
        .assert
        .as_ref()
        .unwrap();

    for &(distro, architecture, scheme, profile) in LANES {
        let overrides = &manifest.distro_overrides[distro];
        assert_eq!(overrides["native_arch"], architecture);
        assert_eq!(overrides["native_scheme"], scheme);
        assert_eq!(overrides["native_profile"], profile);
        assert_eq!(overrides["native_corpus_update_version"], VERSION);
        let variables = build_manifest_variables(&config, distro, &manifest);
        let expanded = expand_assertion(assertion, &variables);
        let expected = expected_rows(architecture, scheme, profile);
        assert_eq!(
            expanded.stdout_json.as_ref().unwrap()[0].expected,
            JsonExpectation::Equals(expected.clone()),
            "{} manifest row",
            distro
        );

        let (_directory, database, repository_id) =
            production_database(architecture, scheme, profile);
        let actual = execute_json_query(&database);
        assert_eq!(actual, expected, "{} current-schema SQL", distro);
        assert!(evaluate_assertion(&expanded, 0, &actual.to_string(), "").is_ok());
        reject_stdout_defects(&expanded, &expected);

        database.execute_batch("SAVEPOINT wrong_checksum").unwrap();
        database
            .execute(
                "UPDATE repository_packages SET checksum = 'wrong-digest' WHERE repository_id = ?1",
                [repository_id],
            )
            .unwrap();
        let wrong_checksum = execute_json_query(&database);
        assert_eq!(wrong_checksum[0]["checksum_matches_fixture"], 0);
        reject(&expanded, &wrong_checksum, "persisted checksum mismatch");
        database
            .execute_batch("ROLLBACK TO wrong_checksum; RELEASE wrong_checksum")
            .unwrap();

        database.execute_batch("SAVEPOINT extra_package").unwrap();
        insert_package(
            &database,
            repository_id,
            architecture,
            scheme,
            "unexpected-package",
        );
        assert_legacy_accepts_extra_row(&database, profile, scheme, architecture, &expanded);
        database
            .execute_batch("ROLLBACK TO extra_package; RELEASE extra_package")
            .unwrap();

        database.execute_batch("SAVEPOINT extra_key").unwrap();
        database
            .execute(
                "INSERT INTO repository_package_keys (repository_id, public_key, status) VALUES (?1, 'second-key', 'retired')",
                [repository_id],
            )
            .unwrap();
        assert_legacy_accepts_extra_row(&database, profile, scheme, architecture, &expanded);
        database
            .execute_batch("ROLLBACK TO extra_key; RELEASE extra_key")
            .unwrap();
    }
}

pub(super) fn assert_tnpm18_repository_shape(manifest: &TestManifest) {
    let test = manifest
        .test
        .iter()
        .find(|test| test.id == "TNPM18")
        .expect("TNPM18");
    assert_eq!(test.step.len(), 10);
    assert!(
        test.step[0]
            .run
            .as_deref()
            .is_some_and(|run| { run.contains("prepare-native-update-repository.sh") })
    );
    assert!(
        test.step[3]
            .run
            .as_deref()
            .is_some_and(|run| run.contains(" update "))
    );
    let step = &test.step[1];
    assert_eq!(
        step.run.as_deref(),
        Some(format!(
            ". /tmp/native-update-repository/native-update-repository.env && sqlite3 -json ${{DB_PATH}} \"{QUERY}\""
        )
        .as_str())
    );
    let assertion = step.assert.as_ref().expect("repository assertion");
    assert_eq!(assertion.exit_code, Some(0));
    assert!(assertion.stdout_contains.is_none());
    assert!(assertion.stdout_contains_all.is_none());
    assert!(assertion.stdout_contains_any.is_none());
    assert!(assertion.stdout_not_contains.is_none());
    let [root] = assertion.stdout_json.as_deref().expect("one JSON check") else {
        panic!("TNPM18 repository must compare one JSON document");
    };
    assert_eq!(root.pointer, "");
    let JsonExpectation::Equals(rows) = &root.expected else {
        panic!("TNPM18 repository must assert exact root JSON");
    };
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(rows[0]["repository_name"], REPOSITORY);
    assert_eq!(rows[0]["checksum_matches_fixture"], 1);
    assert_eq!(rows[0]["key_status"], "active");
}

fn expected_rows(architecture: &str, scheme: &str, profile: &str) -> Value {
    json!([{
        "repository_name": REPOSITORY,
        "default_strategy": "binary",
        "source_profile": profile,
        "package_name": PACKAGE,
        "package_version": VERSION,
        "version_scheme": scheme,
        "architecture": architecture,
        "checksum_matches_fixture": 1,
        "key_status": "active",
    }])
}

fn production_database(
    architecture: &str,
    scheme: &str,
    profile: &str,
) -> (tempfile::TempDir, Connection, i64) {
    let directory = tempfile::tempdir().expect("create current-schema witness directory");
    let path = directory.path().join("state.db");
    db::init(&path).expect("initialize production schema");
    let database = db::open(&path).expect("open production database");
    database
        .execute(
            "INSERT INTO repositories (name, url, default_strategy, source_profile) VALUES (?1, 'https://example.test/repo', 'binary', ?2)",
            params![REPOSITORY, profile],
        )
        .expect("insert fixture repository");
    let repository_id = database.last_insert_rowid();
    insert_package(&database, repository_id, architecture, scheme, PACKAGE);
    database
        .execute(
            "INSERT INTO repository_package_keys (repository_id, public_key, status) VALUES (?1, 'fixture-key', 'active')",
            [repository_id],
        )
        .expect("insert fixture key");
    (directory, database, repository_id)
}

fn insert_package(
    database: &Connection,
    repository_id: i64,
    architecture: &str,
    scheme: &str,
    name: &str,
) {
    let debian_multi_arch = (scheme == "debian").then_some("no");
    database
        .execute(
            "INSERT INTO repository_packages (repository_id, name, version, architecture, checksum, size, download_url, version_scheme, debian_multi_arch) VALUES (?1, ?2, ?3, ?4, ?5, 1, 'https://example.test/package.ccs', ?6, ?7)",
            params![repository_id, name, VERSION, architecture, DIGEST, scheme, debian_multi_arch],
        )
        .expect("insert fixture package");
}

fn execute_json_query(database: &Connection) -> Value {
    let sql = QUERY
        .replace("$NATIVE_UPDATE_REPOSITORY", REPOSITORY)
        .replace("$NATIVE_UPDATE_CCS_SHA256", DIGEST);
    let mut statement = database.prepare(&sql).expect("prepare manifest SQL");
    let columns = statement
        .column_names()
        .iter()
        .map(|name| name.to_string())
        .collect::<Vec<_>>();
    assert_eq!(columns, COLUMNS, "named JSON column order");
    let rows = statement
        .query_map([], |row| {
            let mut object = serde_json::Map::new();
            for (index, name) in columns.iter().enumerate() {
                let value = match row.get_ref(index)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(value) => json!(value),
                    ValueRef::Real(value) => json!(value),
                    ValueRef::Text(value) => json!(std::str::from_utf8(value).unwrap()),
                    ValueRef::Blob(_) => panic!("unexpected blob in {name}"),
                };
                object.insert(name.clone(), value);
            }
            Ok(Value::Object(object))
        })
        .expect("execute manifest SQL")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("read repository rows");
    Value::Array(rows)
}

fn assert_legacy_accepts_extra_row(
    database: &Connection,
    profile: &str,
    scheme: &str,
    architecture: &str,
    assertion: &Assertion,
) {
    let rows = execute_json_query(database);
    assert_eq!(rows.as_array().unwrap().len(), 2);
    reject(assertion, &rows, "extra repository package or key");

    let legacy_rows = database
        .prepare(LEGACY_QUERY)
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(legacy_rows.len(), 2);
    let expected_line = format!(
        "{REPOSITORY}|binary|{}|{PACKAGE}|{VERSION}|{}|{}|{DIGEST}|active",
        profile, scheme, architecture
    );
    assert!(
        legacy_rows.iter().any(|line| line == &expected_line),
        "old grep -Fx accepts the expected line despite another repository row"
    );
}

fn reject_stdout_defects(assertion: &Assertion, expected: &Value) {
    for field in COLUMNS {
        let mut wrong = expected.clone();
        wrong[0][*field] = json!("wrong-value");
        reject(assertion, &wrong, field);
    }
    let mut wrong_type = expected.clone();
    wrong_type[0]["checksum_matches_fixture"] = json!("1");
    reject(assertion, &wrong_type, "string checksum comparison");
    reject(assertion, &json!([]), "missing row");
    reject(
        assertion,
        &json!([expected[0].clone(), expected[0].clone()]),
        "extra row",
    );
    assert!(evaluate_assertion(assertion, 0, "not JSON", "").is_err());
    assert!(evaluate_assertion(assertion, 1, &expected.to_string(), "").is_err());
}

fn reject(assertion: &Assertion, rows: &Value, defect: &str) {
    assert!(
        evaluate_assertion(assertion, 0, &rows.to_string(), "").is_err(),
        "must reject {defect}"
    );
}
