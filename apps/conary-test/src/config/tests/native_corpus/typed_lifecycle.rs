// apps/conary-test/src/config/tests/native_corpus/typed_lifecycle.rs
#![cfg(test)]

use super::super::{load_manifest, remi_manifest_path};
use crate::config::manifest::{Assertion, JsonExpectation, TestStep};
use crate::engine::{
    assertions::evaluate_assertion,
    variables::{expand_assertion, expand_variables},
};
use conary_core::{
    db::{
        models::{Changeset, ChangesetStatus, Trove, TroveType},
        schema,
    },
    repository::{dependency_model::DebianMultiArch, versioning::VersionScheme},
};
use rusqlite::{Connection, params, types::ValueRef};
use serde_json::{Value, json};

const NAME: &str = "phase4-daily-driver-corpus";
const VERSION: &str = "1.0.0-1";
const QUERY: &str = "SELECT t.name AS trove_name,t.version AS trove_version,t.architecture AS trove_architecture,t.source_profile AS trove_source_profile,b.source_format,b.source_family,b.source_profile,b.source_arch,b.source_package,b.source_version,b.scriptlet_fidelity,b.lifecycle_state,(b.installed_changeset_id = t.installed_by_changeset_id) AS same_changeset FROM troves AS t LEFT JOIN installed_native_lifecycle_bundles AS b ON b.trove_id=t.id WHERE t.name='phase4-daily-driver-corpus' ORDER BY t.id";
const COLUMNS: &[&str] = &[
    "trove_name",
    "trove_version",
    "trove_architecture",
    "trove_source_profile",
    "source_format",
    "source_family",
    "source_profile",
    "source_arch",
    "source_package",
    "source_version",
    "scriptlet_fidelity",
    "lifecycle_state",
    "same_changeset",
];

struct Lane {
    distro: &'static str,
    format: &'static str,
    evidence_format: &'static str,
    arch: &'static str,
    profile: &'static str,
    scheme: VersionScheme,
}

const LANES: &[Lane] = &[
    Lane {
        distro: "fedora44",
        format: "rpm",
        evidence_format: "rpm",
        arch: "x86_64",
        profile: "fedora-44",
        scheme: VersionScheme::Rpm,
    },
    Lane {
        distro: "ubuntu-26.04",
        format: "deb",
        evidence_format: "deb",
        arch: "amd64",
        profile: "ubuntu-26.04",
        scheme: VersionScheme::Debian,
    },
    Lane {
        distro: "arch",
        format: "arch",
        evidence_format: "alpm",
        arch: "x86_64",
        profile: "arch",
        scheme: VersionScheme::Arch,
    },
];

#[test]
fn native_corpus_tnpm15_lifecycle_requires_exact_persisted_row() {
    let manifest = load_manifest(&remi_manifest_path(
        "phase4-native-daily-driver-corpus.toml",
    ))
    .expect("load native daily-driver corpus manifest");
    let test = manifest
        .test
        .iter()
        .find(|test| test.id == "TNPM15")
        .expect("TNPM15");
    assert_eq!(test.step.len(), 10, "TNPM15 ten-step order");
    let step = &test.step[7];
    let command = step.run.as_deref().expect("step 8 SQL command");
    assert_eq!(command, format!("sqlite3 -json ${{DB_PATH}} \"{QUERY}\""));
    assert_eq!(command.matches("sqlite3").count(), 1);
    assert_eq!(command.matches("SELECT ").count(), 1);
    assert!(!QUERY.contains(';'), "step 8 runs one SQL statement");
    let query = sqlite_query(step);
    assert_eq!(query, QUERY);
    let assertion = root_assertion(step);

    for lane in LANES {
        let overrides = &manifest.distro_overrides[lane.distro];
        for (key, expected) in [
            ("native_target", lane.format),
            ("native_corpus_source_format", lane.evidence_format),
            ("native_arch", lane.arch),
            ("native_profile", lane.profile),
            ("native_corpus_fixture_version", VERSION),
            ("native_corpus_lifecycle_fidelity", "native-lifecycle"),
        ] {
            assert_eq!(
                overrides.get(key).map(String::as_str),
                Some(expected),
                "{} {key}",
                lane.distro
            );
        }
        assert_eq!(expand_variables(command, overrides), command);
        let expanded = expand_assertion(assertion, overrides);
        let expected = expected_rows(lane);
        assert_eq!(
            expanded.stdout_json.as_ref().unwrap()[0].expected,
            JsonExpectation::Equals(expected.clone()),
            "{} exact root JSON expectation",
            lane.distro
        );

        let (database, target_id) = fixture_database(lane);
        let actual = execute_json_query(&database, query);
        assert_eq!(actual, expected, "{} production-schema row", lane.distro);
        assert!(evaluate_assertion(&expanded, 0, &actual.to_string(), "").is_ok());
        let unrelated: i64 = database
            .query_row(
                "SELECT COUNT(*) FROM installed_native_lifecycle_bundles WHERE trove_id <> ?1",
                [target_id],
                |row| row.get(0),
            )
            .expect("count unrelated bundle");
        assert_eq!(unrelated, 1, "foreign bundle must not enter the target row");
        reject_persisted_defects(lane, query, &expanded);
        reject_bad_stdout(&expanded, &expected);
    }
}

fn sqlite_query(step: &TestStep) -> &str {
    step.run
        .as_deref()
        .and_then(|command| command.strip_prefix("sqlite3 -json ${DB_PATH} \""))
        .and_then(|query| query.strip_suffix('"'))
        .expect("one quoted sqlite3 JSON query")
}

fn root_assertion(step: &TestStep) -> &Assertion {
    let assertion = step.assert.as_ref().expect("step 8 assertion");
    assert_eq!(assertion.exit_code, Some(0));
    assert!(assertion.exit_code_not.is_none());
    assert!(assertion.stdout_contains.is_none() && assertion.stdout_not_contains.is_none());
    assert!(assertion.stdout_contains_all.is_none() && assertion.stdout_contains_any.is_none());
    assert!(assertion.stdout_contains_if_success.is_none());
    assert!(assertion.stdout_contains_any_if_success.is_none());
    assert!(assertion.stderr_contains.is_none() && assertion.stderr_not_contains.is_none());
    let checks = assertion
        .stdout_json
        .as_ref()
        .expect("exact root JSON assertion");
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].pointer, "");
    assertion
}

fn expected_rows(lane: &Lane) -> Value {
    json!([{
        "trove_name": NAME, "trove_version": VERSION, "trove_architecture": lane.arch,
        "trove_source_profile": lane.profile, "source_format": lane.format,
        "source_family": lane.format, "source_profile": lane.profile,
        "source_arch": lane.arch, "source_package": NAME, "source_version": VERSION,
        "scriptlet_fidelity": "native-lifecycle", "lifecycle_state": "installed",
        "same_changeset": 1,
    }])
}

fn fixture_database(lane: &Lane) -> (Connection, i64) {
    let database = Connection::open_in_memory().expect("open disposable database");
    schema::ensure_current(&database).expect("initialize production schema");
    let changeset_id = insert_changeset(&database);
    let target_id = insert_trove(&database, lane, NAME, VERSION, changeset_id);
    insert_bundle(&database, lane, target_id, NAME, VERSION, changeset_id);
    let unrelated_id = insert_trove(&database, lane, "unrelated-package", "8.0-1", changeset_id);
    insert_bundle(
        &database,
        lane,
        unrelated_id,
        "unrelated-package",
        "8.0-1",
        changeset_id,
    );
    (database, target_id)
}

fn insert_changeset(database: &Connection) -> i64 {
    let mut changeset = Changeset::new("Install lifecycle SQL fixture".into());
    let id = changeset
        .insert(database)
        .expect("insert production changeset");
    changeset
        .update_status(database, ChangesetStatus::Applied)
        .expect("apply changeset");
    id
}

fn insert_trove(
    database: &Connection,
    lane: &Lane,
    name: &str,
    version: &str,
    changeset_id: i64,
) -> i64 {
    let mut trove = Trove::new(name.into(), version.into(), TroveType::Package, lane.scheme);
    trove.architecture = Some(lane.arch.into());
    trove.source_profile = Some(lane.profile.into());
    trove.installed_by_changeset_id = Some(changeset_id);
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
    changeset_id: i64,
) {
    database.execute(
        "INSERT INTO installed_native_lifecycle_bundles (trove_id,source_format,source_family,source_profile,source_arch,source_package,source_version,scriptlet_fidelity,lifecycle_state,bundle_toml,installed_changeset_id) VALUES (?1,?2,?3,?4,?5,?6,?7,'native-lifecycle','installed',?8,?9)",
        params![trove_id, lane.format, lane.format, lane.profile, lane.arch, package, version,
            "fixture = 'selected scalar row'", changeset_id],
    ).expect("insert production-schema lifecycle row");
}

fn execute_json_query(database: &Connection, query: &str) -> Value {
    let mut statement = database
        .prepare(query)
        .expect("prepare extracted manifest SQL");
    let columns = statement
        .column_names()
        .iter()
        .map(|name| name.to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        columns.iter().map(String::as_str).collect::<Vec<_>>(),
        COLUMNS
    );
    let rows = statement
        .query_map([], |row| {
            let mut object = serde_json::Map::new();
            for (index, name) in columns.iter().enumerate() {
                let value = match row.get_ref(index)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(value) => json!(value),
                    ValueRef::Real(value) => json!(value),
                    ValueRef::Text(value) => {
                        json!(std::str::from_utf8(value).expect("SQLite text"))
                    }
                    ValueRef::Blob(_) => panic!("unexpected blob in {name}"),
                };
                object.insert(name.clone(), value);
            }
            Ok(Value::Object(object))
        })
        .expect("execute extracted manifest SQL")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("read SQL rows");
    Value::Array(rows)
}

fn reject_persisted_defects(lane: &Lane, query: &str, assertion: &Assertion) {
    for (mutation, defect) in [
        (
            "DELETE FROM installed_native_lifecycle_bundles WHERE trove_id=?1",
            "missing bundle",
        ),
        (
            "UPDATE installed_native_lifecycle_bundles SET source_family='wrong-family' WHERE trove_id=?1",
            "wrong family",
        ),
        (
            "UPDATE installed_native_lifecycle_bundles SET source_format='eopkg',source_profile=NULL WHERE trove_id=?1",
            "wrong source format",
        ),
        (
            "UPDATE installed_native_lifecycle_bundles SET source_profile=NULL WHERE trove_id=?1",
            "NULL source profile",
        ),
        (
            "UPDATE installed_native_lifecycle_bundles SET source_arch='aarch64' WHERE trove_id=?1",
            "wrong source arch",
        ),
        (
            "UPDATE installed_native_lifecycle_bundles SET source_package='wrong-package' WHERE trove_id=?1",
            "wrong package",
        ),
        (
            "UPDATE installed_native_lifecycle_bundles SET source_version='9.9-1' WHERE trove_id=?1",
            "wrong version",
        ),
        (
            "UPDATE installed_native_lifecycle_bundles SET scriptlet_fidelity='native-free' WHERE trove_id=?1",
            "wrong fidelity",
        ),
        (
            "UPDATE installed_native_lifecycle_bundles SET lifecycle_state='triggers-pending' WHERE trove_id=?1",
            "wrong lifecycle state",
        ),
        (
            "UPDATE installed_native_lifecycle_bundles SET installed_changeset_id=NULL WHERE trove_id=?1",
            "missing bundle changeset",
        ),
        (
            "UPDATE troves SET source_profile=NULL WHERE id=?1",
            "NULL trove profile",
        ),
        (
            "UPDATE troves SET architecture='aarch64' WHERE id=?1",
            "wrong trove arch",
        ),
    ] {
        let (database, target_id) = fixture_database(lane);
        assert_eq!(
            database.execute(mutation, [target_id]).expect(defect),
            1,
            "{defect}"
        );
        let actual = execute_json_query(&database, query);
        rejects(assertion, &actual, defect);
        if matches!(defect, "NULL source profile" | "wrong source arch") {
            assert_legacy_passes(&database, lane, assertion, &actual);
        }
    }

    let (database, target_id) = fixture_database(lane);
    let second_changeset_id = insert_changeset(&database);
    database.execute(
        "UPDATE installed_native_lifecycle_bundles SET installed_changeset_id=?1 WHERE trove_id=?2",
        params![second_changeset_id, target_id],
    ).expect("write schema-valid wrong changeset");
    rejects(
        assertion,
        &execute_json_query(&database, query),
        "wrong changeset",
    );

    let (database, _) = fixture_database(lane);
    let second_changeset_id = insert_changeset(&database);
    let second_id = insert_trove(&database, lane, NAME, "9.9-1", second_changeset_id);
    insert_bundle(
        &database,
        lane,
        second_id,
        NAME,
        "9.9-1",
        second_changeset_id,
    );
    let actual = execute_json_query(&database, query);
    assert_eq!(actual.as_array().unwrap().len(), 2);
    rejects(assertion, &actual, "second named trove");
}

fn assert_legacy_passes(database: &Connection, lane: &Lane, assertion: &Assertion, actual: &Value) {
    let count: i64 = database.query_row(
        "SELECT COUNT(*) FROM installed_native_lifecycle_bundles WHERE trove_id=(SELECT id FROM troves WHERE name='phase4-daily-driver-corpus')",
        [], |row| row.get(0),
    ).expect("old lifecycle count");
    let row: String = database.query_row(
        "SELECT source_format || '|' || source_package || '|' || source_version || '|' || scriptlet_fidelity || '|' || lifecycle_state FROM installed_native_lifecycle_bundles WHERE trove_id=(SELECT id FROM troves WHERE name='phase4-daily-driver-corpus')",
        [], |row| row.get(0),
    ).expect("old lifecycle text row");
    let stdout = format!("{count} lifecycle bundles\n{row}");
    let old = Assertion {
        exit_code: Some(0),
        stdout_contains_all: Some(vec![
            "1 lifecycle bundles".into(),
            format!(
                "{}|{NAME}|{VERSION}|native-lifecycle|installed",
                lane.format
            ),
        ]),
        ..Assertion::default()
    };
    assert!(evaluate_assertion(&old, 0, &stdout, "").is_ok());
    rejects(assertion, actual, "retired substring false positive");
}

fn reject_bad_stdout(assertion: &Assertion, expected: &Value) {
    for actual in [
        json!([]),
        json!([expected[0].clone(), expected[0].clone()]),
        json!({"row": expected[0].clone()}),
        json!([{"same_changeset": 1}]),
    ] {
        rejects(assertion, &actual, "wrong shape or count");
    }
    for value in [json!(0), json!("1"), Value::Null] {
        let mut wrong = expected.clone();
        wrong[0]["same_changeset"] = value;
        rejects(assertion, &wrong, "wrong changeset type or value");
    }
    let mut extra_key = expected.clone();
    extra_key[0]["unexpected"] = json!(true);
    rejects(assertion, &extra_key, "unexpected projected field");
    for output in ["not JSON", "[{", &format!("{expected}{expected}")] {
        assert!(evaluate_assertion(assertion, 0, output, "").is_err());
    }
    assert!(evaluate_assertion(assertion, 1, &expected.to_string(), "").is_err());
}

fn rejects(assertion: &Assertion, actual: &Value, defect: &str) {
    assert!(
        evaluate_assertion(assertion, 0, &actual.to_string(), "").is_err(),
        "{defect}"
    );
}
