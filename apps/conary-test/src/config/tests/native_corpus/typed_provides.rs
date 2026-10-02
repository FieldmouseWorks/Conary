// apps/conary-test/src/config/tests/native_corpus/typed_provides.rs
#![cfg(test)]

use super::super::{load_manifest, remi_manifest_path};
use crate::config::manifest::{Assertion, JsonExpectation, TestStep};
use crate::engine::{
    assertions::evaluate_assertion,
    variables::{expand_assertion, expand_variables},
};
use conary_core::{
    db::{
        models::{ProvideEntry, Trove, TroveType},
        schema,
    },
    repository::{
        dependency_model::{
            CapabilityProvenance, DebianMultiArch, ProvideArchitectureQualifier,
            ProvideVersionRelation, ProvidedCapability, RepositoryCapabilityKind,
            SourcePackageFormat,
        },
        versioning::VersionScheme,
    },
};
use rusqlite::{Connection, types::ValueRef};
use serde_json::{Value, json};

const NAME: &str = "phase4-daily-driver-corpus";
const QUERY: &str = "SELECT t.name AS trove_name,t.version AS trove_version,p.capability,p.version,p.version_relation,p.kind,p.version_scheme,p.architecture_qualifier_kind,p.architecture_qualifier,json_extract(p.provenance,'$.role') AS provenance_role,json_type(p.provenance,'$.format') AS provenance_format_type,json_extract(p.provenance,'$.format') AS provenance_format,json_type(p.provenance,'$.record_index') AS provenance_record_index_type FROM troves AS t JOIN provides AS p ON p.trove_id=t.id WHERE t.name='phase4-daily-driver-corpus' AND p.capability=t.name AND p.kind='package' AND (p.version='1.0' OR json_extract(p.provenance,'$.role')='exact-identity') ORDER BY t.id,CASE WHEN json_extract(p.provenance,'$.role')='exact-identity' THEN 0 ELSE 1 END,p.id";
const COLUMNS: &[&str] = &[
    "trove_name",
    "trove_version",
    "capability",
    "version",
    "version_relation",
    "kind",
    "version_scheme",
    "architecture_qualifier_kind",
    "architecture_qualifier",
    "provenance_role",
    "provenance_format_type",
    "provenance_format",
    "provenance_record_index_type",
];

struct Lane {
    distro: &'static str,
    scheme: VersionScheme,
    format: SourcePackageFormat,
    format_name: &'static str,
}

const LANES: &[Lane] = &[
    Lane {
        distro: "fedora44",
        scheme: VersionScheme::Rpm,
        format: SourcePackageFormat::Rpm,
        format_name: "rpm",
    },
    Lane {
        distro: "ubuntu-26.04",
        scheme: VersionScheme::Debian,
        format: SourcePackageFormat::Debian,
        format_name: "debian",
    },
    Lane {
        distro: "arch",
        scheme: VersionScheme::Arch,
        format: SourcePackageFormat::Alpm,
        format_name: "alpm",
    },
];

#[test]
fn native_corpus_tnpm15_provides_require_exact_persisted_rows() {
    let manifest = load_manifest(&remi_manifest_path(
        "phase4-native-daily-driver-corpus.toml",
    ))
    .expect("load native daily-driver corpus manifest");
    let test = manifest
        .test
        .iter()
        .find(|test| test.id == "TNPM15")
        .expect("TNPM15");
    assert_eq!(test.step.len(), 10);
    let step = &test.step[6];
    let query = sqlite_query(step);
    assert_eq!(query, QUERY, "step 7 must use the proved SQL statement");
    assert!(!query.contains(';'), "step 7 must contain one statement");
    assert_eq!(query.matches("SELECT ").count(), 1);

    let assertion = typed_root_assertion(step);
    for lane in LANES {
        let overrides = &manifest.distro_overrides[lane.distro];
        assert_eq!(overrides["native_scheme"], lane.scheme.as_str());
        assert_eq!(
            overrides["native_corpus_capability_format"],
            lane.format_name
        );
        assert_eq!(overrides["native_corpus_fixture_version"], "1.0.0-1");
        let expanded_query = expand_variables(query, overrides);
        assert_eq!(expanded_query, QUERY);
        let expanded = expand_assertion(assertion, overrides);
        let expected = expected_rows(lane);
        assert_eq!(
            expanded.stdout_json.as_ref().unwrap()[0].expected,
            JsonExpectation::Equals(expected.clone()),
            "{} exact root JSON expectation",
            lane.distro
        );

        let (database, _) = fixture_database(lane);
        let actual = execute_json_query(&database, &expanded_query).expect("execute manifest SQL");
        assert_eq!(actual, expected, "{} production insert rows", lane.distro);
        assert!(evaluate_assertion(&expanded, 0, &actual.to_string(), "").is_ok());
        reject_persisted_defects(lane, &expanded_query, &expanded);
        reject_bad_stdout(&expanded, &expected);
    }
}

fn sqlite_query(step: &TestStep) -> &str {
    let command = step.run.as_deref().expect("TNPM15 step 7 SQL command");
    assert_eq!(command.matches("sqlite3").count(), 1);
    command
        .strip_prefix("sqlite3 -json ${DB_PATH} \"")
        .and_then(|query| query.strip_suffix('"'))
        .expect("one sqlite3 JSON query")
}

fn typed_root_assertion(step: &TestStep) -> &Assertion {
    let assertion = step.assert.as_ref().expect("step 7 assertion");
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
    let checks = assertion.stdout_json.as_ref().expect("root JSON assertion");
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].pointer, "");
    assertion
}

fn expected_rows(lane: &Lane) -> Value {
    json!([
        {
            "trove_name": NAME, "trove_version": "1.0.0-1", "capability": NAME,
            "version": "1.0.0-1", "version_relation": "eq", "kind": "package",
            "version_scheme": lane.scheme.as_str(), "architecture_qualifier_kind": "implicit",
            "architecture_qualifier": null, "provenance_role": "exact-identity",
            "provenance_format_type": null, "provenance_format": null,
            "provenance_record_index_type": null,
        },
        {
            "trove_name": NAME, "trove_version": "1.0.0-1", "capability": NAME,
            "version": "1.0", "version_relation": "eq", "kind": "package",
            "version_scheme": lane.scheme.as_str(), "architecture_qualifier_kind": "implicit",
            "architecture_qualifier": null, "provenance_role": "source-declared",
            "provenance_format_type": "text", "provenance_format": lane.format_name,
            "provenance_record_index_type": "integer",
        },
    ])
}

fn fixture_database(lane: &Lane) -> (Connection, i64) {
    let database = Connection::open_in_memory().expect("open disposable SQLite database");
    schema::ensure_current(&database).expect("initialize production schema");
    let target_id = insert_trove(&database, lane, NAME, "1.0.0-1");
    ProvideEntry::insert_package_capabilities(
        &database,
        target_id,
        NAME,
        "1.0.0-1",
        lane.scheme,
        &[
            exact_provide(lane, NAME, "1.0.0-1"),
            source_provide(lane, NAME, 7),
        ],
    )
    .expect("persist target package capabilities");

    let unrelated = "unrelated-package";
    let unrelated_id = insert_trove(&database, lane, unrelated, "8.0-1");
    ProvideEntry::insert_package_capabilities(
        &database,
        unrelated_id,
        unrelated,
        "8.0-1",
        lane.scheme,
        &[
            exact_provide(lane, unrelated, "8.0-1"),
            source_provide(lane, NAME, 42),
        ],
    )
    .expect("persist unrelated package capabilities");
    (database, target_id)
}

fn insert_trove(database: &Connection, lane: &Lane, name: &str, version: &str) -> i64 {
    let mut trove = Trove::new(name.into(), version.into(), TroveType::Package, lane.scheme);
    if lane.scheme == VersionScheme::Debian {
        trove.debian_multi_arch = Some(DebianMultiArch::No);
    }
    trove.insert(database).expect("insert production trove")
}

fn exact_provide(lane: &Lane, name: &str, version: &str) -> ProvidedCapability {
    ProvidedCapability {
        kind: RepositoryCapabilityKind::PackageName,
        name: name.into(),
        version: Some(version.into()),
        version_relation: Some(ProvideVersionRelation::Equal),
        version_scheme: lane.scheme,
        architecture_qualifier: ProvideArchitectureQualifier::Implicit,
        provenance: CapabilityProvenance::ExactIdentity,
    }
}

fn source_provide(lane: &Lane, name: &str, record_index: u32) -> ProvidedCapability {
    ProvidedCapability {
        kind: RepositoryCapabilityKind::PackageName,
        name: name.into(),
        version: Some("1.0".into()),
        version_relation: Some(ProvideVersionRelation::Equal),
        version_scheme: lane.scheme,
        architecture_qualifier: ProvideArchitectureQualifier::Implicit,
        provenance: CapabilityProvenance::SourceDeclared {
            format: lane.format,
            record_index,
        },
    }
}

fn execute_json_query(database: &Connection, query: &str) -> rusqlite::Result<Value> {
    let mut statement = database.prepare(query)?;
    let columns = statement
        .column_names()
        .iter()
        .map(|name| name.to_string())
        .collect::<Vec<_>>();
    assert_eq!(columns, COLUMNS, "manifest SQL column order and aliases");
    let expected = expected_rows(&LANES[0]);
    let expected_keys = expected[0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        columns
            .iter()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>(),
        expected_keys,
        "manifest SQL aliases must equal the JSON key set"
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
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Value::Array(rows))
}

fn reject_persisted_defects(lane: &Lane, query: &str, assertion: &Assertion) {
    for (mutation, defect) in [
        ("DELETE FROM troves WHERE id = ?1", "missing target trove"),
        (
            "DELETE FROM provides WHERE trove_id = ?1 AND version = '1.0'",
            "missing source declaration",
        ),
        (
            "UPDATE provides SET version = '1.1' WHERE trove_id = ?1 AND version = '1.0'",
            "wrong source version",
        ),
        (
            "UPDATE provides SET version_relation = 'ge' WHERE trove_id = ?1 AND version = '1.0'",
            "wrong version relation",
        ),
        (
            "UPDATE provides SET version_scheme = 'conary' WHERE trove_id = ?1 AND version = '1.0'",
            "wrong version scheme",
        ),
        (
            "UPDATE provides SET architecture_qualifier_kind = 'any' WHERE trove_id = ?1 AND version = '1.0'",
            "wrong architecture qualifier kind",
        ),
        (
            "UPDATE provides SET architecture_qualifier_kind = 'exact', architecture_qualifier = 'wrong-arch' WHERE trove_id = ?1 AND version = '1.0'",
            "wrong architecture qualifier",
        ),
        (
            "UPDATE provides SET provenance = json_set(provenance, '$.role', 'author-declared') WHERE trove_id = ?1 AND version = '1.0'",
            "wrong source role",
        ),
        (
            "UPDATE provides SET provenance = json_set(provenance, '$.format', 'ccs') WHERE trove_id = ?1 AND version = '1.0'",
            "wrong source format",
        ),
        (
            "UPDATE provides SET provenance = json_set(provenance, '$.record_index', 'wrong') WHERE trove_id = ?1 AND version = '1.0'",
            "wrong source record index type",
        ),
        (
            "UPDATE provides SET provenance = json_set(provenance, '$.format', json('null')) WHERE trove_id = ?1 AND version = '1.0.0-1'",
            "explicit JSON null instead of absent format",
        ),
        (
            "UPDATE provides SET provenance = json_set(provenance, '$.record_index', json('null')) WHERE trove_id = ?1 AND version = '1.0.0-1'",
            "explicit JSON null instead of absent record index",
        ),
        (
            "UPDATE provides SET provenance = json_set(provenance, '$.role', 'author-declared') WHERE trove_id = ?1 AND version = '1.0.0-1'",
            "missing exact identity role",
        ),
    ] {
        let (database, target_id) = fixture_database(lane);
        database.execute(mutation, [target_id]).expect(defect);
        let actual = execute_json_query(&database, query).expect(defect);
        rejects(assertion, &actual, defect);
    }

    let (database, _) = fixture_database(lane);
    let second_id = insert_trove(&database, lane, NAME, "2.0.0-1");
    ProvideEntry::insert_package_capabilities(
        &database,
        second_id,
        NAME,
        "2.0.0-1",
        lane.scheme,
        &[
            exact_provide(lane, NAME, "2.0.0-1"),
            source_provide(lane, NAME, 9),
        ],
    )
    .expect("persist second same-name version");
    let actual = execute_json_query(&database, query).expect("query both versions");
    assert_eq!(actual.as_array().unwrap().len(), 4);
    rejects(assertion, &actual, "second same-name version");

    let (database, target_id) = fixture_database(lane);
    ProvideEntry::insert_package_capabilities(
        &database,
        target_id,
        NAME,
        "1.0.0-1",
        lane.scheme,
        &[
            exact_provide(lane, NAME, "1.0.0-1"),
            source_provide(lane, NAME, 8),
        ],
    )
    .expect("a distinct source record index is valid persisted state");
    let actual = execute_json_query(&database, query).expect("query duplicate source declaration");
    assert_eq!(actual.as_array().unwrap().len(), 3);
    rejects(assertion, &actual, "duplicate 1.0 source declaration");
    demonstrate_legacy_false_positive(&database, lane, assertion);
}

fn demonstrate_legacy_false_positive(database: &Connection, lane: &Lane, assertion: &Assertion) {
    let mut statement = database.prepare(
        "SELECT capability || '|' || version || '|' || version_relation || '|' || kind || '|' || version_scheme || '|' || json_extract(provenance, '$.role') || '|' || COALESCE(json_extract(provenance, '$.format'), '') FROM provides WHERE trove_id = (SELECT id FROM troves WHERE name = 'phase4-daily-driver-corpus') AND capability = 'phase4-daily-driver-corpus' AND kind = 'package' ORDER BY json_extract(provenance, '$.role')"
    ).expect("prepare retired substring projection");
    let stdout = statement
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query retired substring projection")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("read retired substring projection")
        .join("\n");
    let legacy = Assertion {
        exit_code: Some(0),
        stdout_contains_all: Some(vec![
            format!(
                "{NAME}|1.0.0-1|eq|package|{}|exact-identity|",
                lane.scheme.as_str()
            ),
            format!(
                "{NAME}|1.0|eq|package|{}|source-declared|{}",
                lane.scheme.as_str(),
                lane.format_name
            ),
        ]),
        ..Assertion::default()
    };
    assert!(evaluate_assertion(&legacy, 0, &stdout, "").is_ok());
    assert!(evaluate_assertion(assertion, 0, &stdout, "").is_err());
}

fn reject_bad_stdout(assertion: &Assertion, expected: &Value) {
    rejects(assertion, &json!([]), "no rows");
    let mut one_row = expected.clone();
    one_row.as_array_mut().unwrap().pop();
    rejects(assertion, &one_row, "missing source row");
    let mut extra_row = expected.clone();
    extra_row.as_array_mut().unwrap().push(expected[1].clone());
    rejects(assertion, &extra_row, "extra row");
    let mut extra_field = expected.clone();
    extra_field[0]["unexpected"] = json!(true);
    rejects(assertion, &extra_field, "extra field");
    let mut missing_field = expected.clone();
    missing_field[0]
        .as_object_mut()
        .unwrap()
        .remove("provenance_role");
    rejects(assertion, &missing_field, "missing field");
    let mut wrong_type = expected.clone();
    wrong_type[1]["provenance_record_index_type"] = json!(1);
    rejects(assertion, &wrong_type, "wrong JSON type");
    let mut explicit_null = expected.clone();
    explicit_null[0]["provenance_format_type"] = json!("null");
    rejects(assertion, &explicit_null, "SQL NULL versus JSON null");
    for stdout in [
        "not JSON",
        &format!("{expected}{expected}"),
        &format!("{expected} trailing"),
    ] {
        assert!(evaluate_assertion(assertion, 0, stdout, "").is_err());
    }
    assert!(evaluate_assertion(assertion, 1, &expected.to_string(), "").is_err());
}

fn rejects(assertion: &Assertion, actual: &Value, defect: &str) {
    assert!(
        evaluate_assertion(assertion, 0, &actual.to_string(), "").is_err(),
        "must reject {defect}: {actual}"
    );
}
