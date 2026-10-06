// apps/conary-test/src/config/tests/native_corpus/typed_update_lifecycle.rs
#![cfg(test)]

use super::super::{load_manifest, remi_manifest_path};
use crate::config::{
    load_global_config,
    manifest::{Assertion, JsonExpectation},
};
use crate::engine::{
    assertions::evaluate_assertion,
    variables::{build_manifest_variables, expand_assertion, expand_variables},
};
use conary_core::{
    db::{
        self,
        models::{Trove, TroveType},
    },
    repository::{dependency_model::DebianMultiArch, versioning::VersionScheme},
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{
    path::Path,
    process::{Command, Output},
};

const NAME: &str = "phase4-daily-driver-corpus";
const VERSION: &str = "1.0.1-1";
const SOURCE_ENV: &str = "/tmp/native-update-repository/native-update-repository.env";
const SOURCE_DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const CCS_DIGEST: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
const WRONG_DIGEST: &str = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
const SCHEMA: &str = "conary.native-lifecycles.v1";
const REVISION: u16 = 20;

struct Lane {
    distro: &'static str,
    format: &'static str,
    evidence_format: &'static str,
    architecture: &'static str,
    scheme: VersionScheme,
    profile: &'static str,
}

const LANES: &[Lane] = &[
    Lane {
        distro: "fedora44",
        format: "rpm",
        evidence_format: "rpm",
        architecture: "x86_64",
        scheme: VersionScheme::Rpm,
        profile: "fedora-44",
    },
    Lane {
        distro: "ubuntu-26.04",
        format: "deb",
        evidence_format: "deb",
        architecture: "amd64",
        scheme: VersionScheme::Debian,
        profile: "ubuntu-26.04",
    },
    Lane {
        distro: "arch",
        format: "arch",
        evidence_format: "alpm",
        architecture: "x86_64",
        scheme: VersionScheme::Arch,
        profile: "arch",
    },
];

#[test]
fn native_corpus_tnpm18_lifecycle_requires_parsed_native_checksum_and_exact_rows() {
    let manifest = load_manifest(&remi_manifest_path(
        "phase4-native-daily-driver-corpus.toml",
    ))
    .expect("load daily-driver manifest");
    let config = load_global_config(&remi_manifest_path("../config.toml"))
        .expect("load integration distro config");
    let test = manifest
        .test
        .iter()
        .find(|test| test.id == "TNPM18")
        .expect("TNPM18");
    assert_eq!(test.step.len(), 10);
    let step = &test.step[8];
    let run = step.run.as_deref().expect("lifecycle projection command");
    assert_eq!(run.matches(SOURCE_ENV).count(), 1);
    assert_eq!(run.matches("python3 -c").count(), 1);
    assert_eq!(run.matches("SELECT ").count(), 1);
    assert!(run.contains("WHERE t.name = ? ORDER BY t.id"));
    assert!(run.contains("LEFT JOIN installed_native_lifecycle_bundles"));
    assert!(run.contains("tomllib.loads(bundle_toml)"));
    assert!(run.contains("' \"${DB_PATH}\" \"$NATIVE_UPDATE_SOURCE_SHA256\""));
    assert!(!run.contains("grep -F") && !run.contains("NATIVE_UPDATE_CCS_SHA256"));
    let assertion = step.assert.as_ref().expect("lifecycle assertion");
    assert_eq!(assertion.exit_code, Some(0));
    assert!(assertion.stdout_contains.is_none());
    assert!(assertion.stdout_contains_all.is_none());
    assert!(assertion.stdout_contains_any.is_none());
    assert!(assertion.stdout_not_contains.is_none());
    let [root] = assertion
        .stdout_json
        .as_deref()
        .expect("one typed JSON assertion")
    else {
        panic!("lifecycle step must compare one JSON document");
    };
    assert_eq!(root.pointer, "");
    let JsonExpectation::Equals(rows) = &root.expected else {
        panic!("lifecycle step must assert exact root JSON");
    };
    assert_eq!(rows.as_array().unwrap().len(), 1);

    for lane in LANES {
        let overrides = &manifest.distro_overrides[lane.distro];
        for (key, expected) in [
            ("native_target", lane.format),
            ("native_corpus_source_format", lane.evidence_format),
            ("native_arch", lane.architecture),
            ("native_scheme", lane.scheme.as_str()),
            ("native_profile", lane.profile),
            ("native_corpus_update_version", VERSION),
        ] {
            assert_eq!(overrides[key], expected, "{} {key}", lane.distro);
        }
        let directory = tempfile::tempdir().expect("current-schema witness directory");
        let db_path = directory.path().join("state.db");
        let database = production_database(&db_path);
        let target_id = insert_trove(&database, lane, NAME, VERSION);
        insert_bundle(
            &database,
            lane,
            target_id,
            NAME,
            VERSION,
            &bundle_toml(SOURCE_DIGEST),
        );
        let unrelated_id = insert_trove(&database, lane, "unrelated-package", "8.0-1");
        insert_bundle(
            &database,
            lane,
            unrelated_id,
            "unrelated-package",
            "8.0-1",
            &bundle_toml(WRONG_DIGEST),
        );

        let env_path = directory.path().join("native-update-repository.env");
        std::fs::write(&env_path, format!(
            "NATIVE_UPDATE_SOURCE_SHA256={SOURCE_DIGEST}\nNATIVE_UPDATE_CCS_SHA256={CCS_DIGEST}\n"
        )).expect("write distinct native and CCS fixture digests");
        assert_ne!(SOURCE_DIGEST, CCS_DIGEST);
        let mut variables = build_manifest_variables(&config, lane.distro, &manifest);
        variables.insert("DB_PATH".into(), db_path.to_str().unwrap().into());
        let command =
            expand_variables(run, &variables).replacen(SOURCE_ENV, env_path.to_str().unwrap(), 1);
        let expanded = expand_assertion(assertion, &variables);
        let expected = expected_rows(lane);
        assert_eq!(
            expanded.stdout_json.as_ref().unwrap()[0].expected,
            JsonExpectation::Equals(expected.clone()),
            "{} literal manifest row",
            lane.distro
        );
        assert_accepts(&command, &expanded, &expected);

        set_bundle(
            &database,
            target_id,
            &format!(
                "schema = \"{SCHEMA}\"\nschema_revision = {REVISION}\n# source_checksum = \"sha256:{SOURCE_DIGEST}\"\n"
            ),
        );
        assert_nonzero(&command, &expanded, "checksum only in a TOML comment");
        set_bundle(&database, target_id, &bundle_toml(SOURCE_DIGEST));

        set_bundle(&database, target_id, &bundle_toml(WRONG_DIGEST));
        let wrong = assert_rejects_json(&command, &expanded, "wrong top-level digest");
        assert_eq!(wrong[0]["source_checksum_matches_fixture"], 0);
        set_bundle(&database, target_id, &bundle_toml(SOURCE_DIGEST));

        let extra_id = insert_trove(&database, lane, NAME, "0.9.0-1");
        let extra = assert_rejects_json(&command, &expanded, "extra same-name trove");
        assert_eq!(extra.as_array().unwrap().len(), 2);
        assert_eq!(extra[0], expected[0]);
        assert_eq!(extra[1]["source_format"], Value::Null);
        database
            .execute("DELETE FROM troves WHERE id = ?1", [extra_id])
            .unwrap();

        database
            .execute(
                "DELETE FROM installed_native_lifecycle_bundles WHERE trove_id = ?1",
                [target_id],
            )
            .unwrap();
        let missing = assert_rejects_json(&command, &expanded, "missing lifecycle bundle");
        assert_eq!(missing.as_array().unwrap().len(), 1);
        for field in [
            "source_format",
            "source_package",
            "source_version",
            "lifecycle_state",
            "bundle_schema",
            "bundle_schema_revision",
        ] {
            assert_eq!(missing[0][field], Value::Null, "{field} on missing bundle");
        }
        assert_eq!(missing[0]["source_checksum_matches_fixture"], 0);
        insert_bundle(
            &database,
            lane,
            target_id,
            NAME,
            VERSION,
            &bundle_toml(SOURCE_DIGEST),
        );

        set_bundle(&database, target_id, "schema = [\n");
        assert_nonzero(&command, &expanded, "malformed TOML");
    }
}

fn expected_rows(lane: &Lane) -> Value {
    json!([{
        "trove_name": NAME,
        "trove_version": VERSION,
        "trove_architecture": lane.architecture,
        "trove_version_scheme": lane.scheme.as_str(),
        "trove_source_profile": lane.profile,
        "source_format": lane.format,
        "source_package": NAME,
        "source_version": VERSION,
        "lifecycle_state": "installed",
        "bundle_schema": SCHEMA,
        "bundle_schema_revision": REVISION,
        "source_checksum_matches_fixture": 1,
    }])
}

fn production_database(path: &Path) -> Connection {
    db::init(path).expect("initialize production schema");
    db::open(path).expect("open production database")
}

fn insert_trove(database: &Connection, lane: &Lane, name: &str, version: &str) -> i64 {
    let mut trove = Trove::new(name.into(), version.into(), TroveType::Package, lane.scheme);
    trove.architecture = Some(lane.architecture.into());
    trove.source_profile = Some(lane.profile.into());
    if lane.scheme == VersionScheme::Debian {
        trove.debian_multi_arch = Some(DebianMultiArch::No);
    }
    trove.insert(database).expect("insert production trove")
}

fn insert_bundle(
    database: &Connection,
    lane: &Lane,
    trove_id: i64,
    package: &str,
    version: &str,
    toml: &str,
) {
    database.execute(
        "INSERT INTO installed_native_lifecycle_bundles (trove_id, source_format, source_family, source_package, source_version, scriptlet_fidelity, lifecycle_state, bundle_toml) VALUES (?1, ?2, ?2, ?3, ?4, 'native-lifecycle', 'installed', ?5)",
        params![trove_id, lane.format, package, version, toml],
    ).expect("insert production lifecycle bundle");
}

fn bundle_toml(digest: &str) -> String {
    format!(
        "schema = \"{SCHEMA}\"\nschema_revision = {REVISION}\nsource_checksum = \"sha256:{digest}\"\n"
    )
}

fn set_bundle(database: &Connection, trove_id: i64, toml: &str) {
    database
        .execute(
            "UPDATE installed_native_lifecycle_bundles SET bundle_toml = ?1 WHERE trove_id = ?2",
            params![toml, trove_id],
        )
        .unwrap();
}

fn run_command(command: &str) -> Output {
    Command::new("sh")
        .arg("-c")
        .arg(command)
        .output()
        .expect("run loaded manifest command")
}

fn assert_accepts(command: &str, assertion: &Assertion, expected: &Value) {
    let output = run_command(command);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let actual: Value = serde_json::from_slice(&output.stdout).expect("typed projection JSON");
    assert_eq!(&actual, expected);
    assert!(
        evaluate_assertion(
            assertion,
            0,
            &String::from_utf8_lossy(&output.stdout),
            &String::from_utf8_lossy(&output.stderr)
        )
        .is_ok()
    );
}

fn assert_rejects_json(command: &str, assertion: &Assertion, defect: &str) -> Value {
    let output = run_command(command);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{defect}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let actual: Value =
        serde_json::from_slice(&output.stdout).expect("typed defect projection JSON");
    assert!(
        evaluate_assertion(
            assertion,
            0,
            &String::from_utf8_lossy(&output.stdout),
            &String::from_utf8_lossy(&output.stderr)
        )
        .is_err(),
        "must reject {defect}"
    );
    actual
}

fn assert_nonzero(command: &str, assertion: &Assertion, defect: &str) {
    let output = run_command(command);
    assert!(!output.status.success(), "{defect} must fail command");
    assert!(
        output.stdout.is_empty(),
        "{defect} must not emit partial JSON"
    );
    assert!(
        evaluate_assertion(
            assertion,
            output.status.code().unwrap_or(-1),
            &String::from_utf8_lossy(&output.stdout),
            &String::from_utf8_lossy(&output.stderr)
        )
        .is_err(),
        "must reject {defect}"
    );
}
