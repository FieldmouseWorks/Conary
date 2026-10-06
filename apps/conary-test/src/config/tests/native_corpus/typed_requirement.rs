// apps/conary-test/src/config/tests/native_corpus/typed_requirement.rs
#![cfg(test)]

use super::super::{load_manifest, remi_manifest_path};
use crate::config::manifest::{Assertion, JsonExpectation, TestStep};
use crate::engine::{
    assertions::evaluate_assertion,
    variables::{expand_assertion, expand_variables},
};
use conary_core::{
    db::models::InstalledRequirementGroup,
    repository::{
        dependency_model::RepositoryRequirementKind, requirement::parse_native_requirement,
        versioning::VersionScheme,
    },
};
use rusqlite::{Connection, params, types::ValueRef};
use serde_json::{Value, json};

const NAME: &str = "phase4-daily-driver-corpus";
const TARGET: &str = "phase4-repository-fixture";
const COLUMNS: &[&str] = &[
    "trove_name",
    "trove_version",
    "trove_type",
    "trove_architecture",
    "trove_scheme",
    "source_profile",
    "install_source",
    "install_reason",
    "group_kind",
    "group_scheme",
    "group_type",
    "payload_kind",
    "behavior",
    "description_type",
    "expression_type",
    "expression_operator",
    "operand_type",
    "expression_name",
    "expression_constraint",
    "expression_capability_kind_type",
    "expression_capability_kind",
    "expression_architecture_kind",
    "expression_native_text_type",
    "expression_native_text",
    "alternatives_type",
    "alternatives_count",
    "alternative_type",
    "alternative_name",
    "alternative_constraint",
    "alternative_capability_kind_type",
    "alternative_capability_kind",
    "alternative_architecture_kind",
    "alternative_native_text_type",
    "alternative_native_text",
    "native_text_type",
    "native_text",
    "count_delta",
];

struct Lane {
    distro: &'static str,
    scheme: VersionScheme,
    architecture: &'static str,
    profile: &'static str,
    count: usize,
    requirement_text: &'static str,
    atom_text_type: &'static str,
    atom_text: &'static str,
}

const LANES: &[Lane] = &[
    Lane {
        distro: "fedora44",
        scheme: VersionScheme::Rpm,
        architecture: "x86_64",
        profile: "fedora-44",
        count: 4,
        requirement_text: "phase4-repository-fixture = 1.0.0",
        atom_text_type: "null",
        atom_text: "",
    },
    Lane {
        distro: "ubuntu-26.04",
        scheme: VersionScheme::Debian,
        architecture: "amd64",
        profile: "ubuntu-26.04",
        count: 1,
        requirement_text: "phase4-repository-fixture (= 1.0.0)",
        atom_text_type: "text",
        atom_text: "phase4-repository-fixture (= 1.0.0)",
    },
    Lane {
        distro: "arch",
        scheme: VersionScheme::Arch,
        architecture: "x86_64",
        profile: "arch",
        count: 1,
        requirement_text: "phase4-repository-fixture=1.0.0",
        atom_text_type: "null",
        atom_text: "",
    },
];

#[test]
fn native_corpus_tnpm15_requirement_group_requires_exact_persisted_row() {
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
    let step = &test.step[4];
    let query = sqlite_query(step);
    assert!(
        !query.contains(';'),
        "step 5 must execute one SQL statement"
    );
    assert!(query.contains("LEFT JOIN package_requirement_groups AS g ON g.trove_id = t.id AND json_extract(g.requirement_json, '$.expression.operands.name') = 'phase4-repository-fixture'"));
    assert!(query.contains("COUNT(*) - ${native_corpus_dependency_count} FROM package_requirement_groups AS all_groups WHERE all_groups.trove_id = t.id"));
    assert!(query.contains("WHERE t.name = 'phase4-daily-driver-corpus' ORDER BY t.id, g.id"));
    assert!(
        !query.contains("g.kind =") && !query.contains("g.kind IN"),
        "a kind filter would hide an optional target row"
    );
    let assertion = typed_root_assertion(step);

    for lane in LANES {
        let overrides = &manifest.distro_overrides[lane.distro];
        for (key, value) in [
            ("native_corpus_dependency_count", lane.count.to_string()),
            (
                "native_corpus_requirement_text",
                lane.requirement_text.to_owned(),
            ),
            (
                "native_corpus_atom_text_type",
                lane.atom_text_type.to_owned(),
            ),
            ("native_corpus_atom_text", lane.atom_text.to_owned()),
        ] {
            assert_eq!(overrides.get(key), Some(&value), "{} {key}", lane.distro);
        }
        assert!(!overrides.contains_key("native_corpus_dependency_probe"));
        let query = expand_variables(query, overrides);
        let assertion = expand_assertion(assertion, overrides);
        let expected = expected_rows(lane);
        assert_eq!(
            assertion.stdout_json.as_ref().unwrap()[0].expected,
            JsonExpectation::Equals(expected.clone()),
            "{} assertion",
            lane.distro
        );
        let database = fixture_database(lane);
        let actual = execute_json_query(&database, &query).expect("execute exact manifest SQL");
        assert_eq!(
            actual, expected,
            "{} parsed persisted requirement",
            lane.distro
        );
        assert!(evaluate_assertion(&assertion, 0, &actual.to_string(), "").is_ok());
        reject_persisted_mutations(lane, &query, &assertion);
        reject_bad_output(&assertion, &expected);
    }
}

fn sqlite_query(step: &TestStep) -> &str {
    let command = step.run.as_deref().expect("TNPM15 step 5 command");
    assert_eq!(command.matches("sqlite3").count(), 1);
    command
        .strip_prefix("sqlite3 -json ${DB_PATH} \"")
        .and_then(|query| query.strip_suffix('"'))
        .expect("one quoted sqlite3 -json command")
}

fn typed_root_assertion(step: &TestStep) -> &Assertion {
    let assertion = step.assert.as_ref().expect("step 5 exact assertion");
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
        .expect("exact root JSON equality");
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].pointer, "");
    assertion
}

fn expected_rows(lane: &Lane) -> Value {
    json!([{
        "trove_name": NAME, "trove_version": "1.0.0-1", "trove_type": "package",
        "trove_architecture": lane.architecture, "trove_scheme": lane.scheme.as_str(),
        "source_profile": lane.profile, "install_source": "file", "install_reason": "explicit",
        "group_kind": "depends", "group_scheme": lane.scheme.as_str(), "group_type": "object",
        "payload_kind": "Depends", "behavior": "Hard", "description_type": "null",
        "expression_type": "object", "expression_operator": "atom", "operand_type": "object",
        "expression_name": TARGET, "expression_constraint": "= 1.0.0",
        "expression_capability_kind_type": "null", "expression_capability_kind": null,
        "expression_architecture_kind": "unqualified",
        "expression_native_text_type": lane.atom_text_type,
        "expression_native_text": lane.atom_text,
        "alternatives_type": "array", "alternatives_count": 1, "alternative_type": "object",
        "alternative_name": TARGET, "alternative_constraint": "= 1.0.0",
        "alternative_capability_kind_type": "null", "alternative_capability_kind": null,
        "alternative_architecture_kind": "unqualified",
        "alternative_native_text_type": lane.atom_text_type,
        "alternative_native_text": lane.atom_text,
        "native_text_type": "text", "native_text": lane.requirement_text, "count_delta": 0,
    }])
}

fn fixture_database(lane: &Lane) -> Connection {
    let database = Connection::open_in_memory().expect("open disposable SQLite database");
    database.execute_batch(
        "PRAGMA foreign_keys = ON;
         CREATE TABLE troves (id INTEGER PRIMARY KEY, name TEXT NOT NULL, version TEXT NOT NULL,
             package_release TEXT, type TEXT NOT NULL, architecture TEXT, version_scheme TEXT NOT NULL,
             source_profile TEXT, install_source TEXT NOT NULL, install_reason TEXT NOT NULL);
         CREATE UNIQUE INDEX idx_troves_exact_identity ON troves(name, version,
             COALESCE(package_release, ''), COALESCE(architecture, ''));
         CREATE TABLE package_requirement_groups (id INTEGER PRIMARY KEY, trove_id INTEGER NOT NULL
             REFERENCES troves(id) ON DELETE CASCADE, kind TEXT NOT NULL CHECK(kind IN
             ('depends', 'pre_depends', 'optional', 'build', 'conflict', 'breaks', 'replace', 'obsolete')),
             version_scheme TEXT NOT NULL, requirement_json TEXT NOT NULL,
             UNIQUE(trove_id, kind, requirement_json));"
    ).expect("create installed requirement schema");
    insert_trove(&database, 1, NAME, "1.0.0-1", lane);
    insert_trove(&database, 2, "unrelated-package", "8.0.0-1", lane);
    let mut requirements = vec![
        parse_native_requirement(
            RepositoryRequirementKind::Depends,
            lane.scheme,
            lane.requirement_text,
        )
        .expect("parse source-native target requirement"),
    ];
    if lane.distro == "fedora44" {
        for source_text in ["bash", "coreutils", "glibc"] {
            requirements.push(
                parse_native_requirement(
                    RepositoryRequirementKind::Depends,
                    lane.scheme,
                    source_text,
                )
                .expect("parse non-target RPM count fixture"),
            );
        }
    }
    InstalledRequirementGroup::insert_groups(&database, 1, lane.scheme, &requirements)
        .expect("persist parsed native groups");
    let unrelated = parse_native_requirement(
        RepositoryRequirementKind::Depends,
        lane.scheme,
        "unrelated-dependency",
    )
    .expect("parse unrelated trove group");
    InstalledRequirementGroup::insert_groups(&database, 2, lane.scheme, &[unrelated])
        .expect("persist unrelated group");
    database
}

fn insert_trove(database: &Connection, id: i64, name: &str, version: &str, lane: &Lane) {
    database
        .execute(
            "INSERT INTO troves (id, name, version, type, architecture, version_scheme,
         source_profile, install_source, install_reason) VALUES (?1, ?2, ?3, 'package', ?4, ?5,
         ?6, 'file', 'explicit')",
            params![
                id,
                name,
                version,
                lane.architecture,
                lane.scheme.as_str(),
                lane.profile
            ],
        )
        .expect("insert trove identity");
}

fn execute_json_query(database: &Connection, query: &str) -> rusqlite::Result<Value> {
    let mut statement = database.prepare(query)?;
    let columns = statement
        .column_names()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    assert_eq!(
        columns, COLUMNS,
        "manifest must project every named field in order"
    );
    let rows = statement
        .query_map([], |row| {
            let mut object = serde_json::Map::new();
            for (index, name) in columns.iter().enumerate() {
                let value = match row.get_ref(index)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(number) => json!(number),
                    ValueRef::Real(number) => json!(number),
                    ValueRef::Text(text) => json!(std::str::from_utf8(text).expect("SQLite text")),
                    ValueRef::Blob(_) => panic!("unexpected blob in {name}"),
                };
                object.insert(name.clone(), value);
            }
            Ok(Value::Object(object))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Value::Array(rows))
}

fn reject_persisted_mutations(lane: &Lane, query: &str, assertion: &Assertion) {
    let target = "WHERE trove_id = 1 AND json_extract(requirement_json, '$.expression.operands.name') = 'phase4-repository-fixture'";
    for (mutation, defect) in [
        (
            format!(
                "UPDATE package_requirement_groups SET kind = 'optional', requirement_json = json_set(requirement_json, '$.kind', 'Optional') {target}"
            ),
            "optional target",
        ),
        (
            format!("UPDATE package_requirement_groups SET version_scheme = 'conary' {target}"),
            "wrong group scheme",
        ),
        (
            format!(
                "UPDATE package_requirement_groups SET requirement_json = json_set(requirement_json, '$.kind', 'Optional') {target}"
            ),
            "wrong payload kind",
        ),
        (
            format!(
                "UPDATE package_requirement_groups SET requirement_json = json_set(requirement_json, '$.behavior', 'Conditional') {target}"
            ),
            "wrong behavior",
        ),
        (
            format!(
                "UPDATE package_requirement_groups SET requirement_json = json_set(requirement_json, '$.description', 'optional') {target}"
            ),
            "wrong description type",
        ),
        (
            format!(
                "UPDATE package_requirement_groups SET requirement_json = json_set(requirement_json, '$.expression.operator', 'or') {target}"
            ),
            "wrong expression operator",
        ),
        (
            format!(
                "UPDATE package_requirement_groups SET requirement_json = json_set(requirement_json, '$.expression.operands.version_constraint', '< 1.0.0') {target}"
            ),
            "wrong expression constraint",
        ),
        (
            format!(
                "UPDATE package_requirement_groups SET requirement_json = json_set(requirement_json, '$.expression.operands.capability_kind', 'PackageName') {target}"
            ),
            "wrong expression capability",
        ),
        (
            format!(
                "UPDATE package_requirement_groups SET requirement_json = json_set(requirement_json, '$.expression.operands.architecture_qualifier.kind', 'native') {target}"
            ),
            "wrong expression architecture",
        ),
        (
            format!(
                "UPDATE package_requirement_groups SET requirement_json = json_set(requirement_json, '$.expression.operands.native_text', 'wrong') {target}"
            ),
            "wrong expression native text",
        ),
        (
            format!(
                "UPDATE package_requirement_groups SET requirement_json = json_remove(requirement_json, '$.expression.operands.native_text') {target}"
            ),
            "missing expression native text key",
        ),
        (
            format!(
                "UPDATE package_requirement_groups SET requirement_json = json_set(requirement_json, '$.alternatives[0].name', 'wrong') {target}"
            ),
            "wrong alternative name",
        ),
        (
            format!(
                "UPDATE package_requirement_groups SET requirement_json = json_set(requirement_json, '$.alternatives[0].version_constraint', '< 1.0.0') {target}"
            ),
            "wrong alternative constraint",
        ),
        (
            format!(
                "UPDATE package_requirement_groups SET requirement_json = json_set(requirement_json, '$.alternatives[0].capability_kind', 'PackageName') {target}"
            ),
            "wrong alternative capability",
        ),
        (
            format!(
                "UPDATE package_requirement_groups SET requirement_json = json_set(requirement_json, '$.alternatives[0].architecture_qualifier.kind', 'native') {target}"
            ),
            "wrong alternative architecture",
        ),
        (
            format!(
                "UPDATE package_requirement_groups SET requirement_json = json_set(requirement_json, '$.alternatives[0].native_text', 'wrong') {target}"
            ),
            "wrong alternative native text",
        ),
        (
            format!(
                "UPDATE package_requirement_groups SET requirement_json = json_remove(requirement_json, '$.alternatives[0].native_text') {target}"
            ),
            "missing alternative native text key",
        ),
        (
            format!(
                "UPDATE package_requirement_groups SET requirement_json = json_set(requirement_json, '$.alternatives', json_array(1, 2)) {target}"
            ),
            "extra alternatives",
        ),
        (
            format!(
                "UPDATE package_requirement_groups SET requirement_json = json_set(requirement_json, '$.alternatives', json_object('not', 'an array')) {target}"
            ),
            "wrong alternatives type",
        ),
        (
            format!(
                "UPDATE package_requirement_groups SET requirement_json = json_set(requirement_json, '$.native_text', 'wrong') {target}"
            ),
            "wrong group native text",
        ),
        (
            format!("DELETE FROM package_requirement_groups {target}"),
            "missing target",
        ),
    ] {
        let database = fixture_database(lane);
        database.execute_batch(&mutation).expect(defect);
        if defect == "optional target" {
            let stored = InstalledRequirementGroup::find_by_trove(&database, 1)
                .expect("optional mutation remains a valid typed group");
            assert!(
                stored
                    .iter()
                    .any(|group| group.kind == RepositoryRequirementKind::Optional)
            );
            let old = legacy_assertion(lane);
            assert!(
                evaluate_assertion(&old, 0, &legacy_stdout(&database), "").is_ok(),
                "the three old substrings accept a valid optional target"
            );
        }
        let actual = execute_json_query(&database, query).expect(defect);
        rejects(assertion, &actual, defect);
    }
    for defect in [
        "wrong total count",
        "duplicate target",
        "duplicate same-name trove",
        "wrong trove identity",
    ] {
        let database = fixture_database(lane);
        match defect {
            "wrong total count" => {
                if lane.count == 4 {
                    database.execute("DELETE FROM package_requirement_groups WHERE trove_id = 1 AND id = (SELECT MAX(id) FROM package_requirement_groups WHERE trove_id = 1)", []).unwrap();
                } else {
                    let extra = parse_native_requirement(
                        RepositoryRequirementKind::Depends,
                        lane.scheme,
                        "extra-dependency",
                    )
                    .unwrap();
                    InstalledRequirementGroup::insert_groups(&database, 1, lane.scheme, &[extra])
                        .unwrap();
                }
            }
            "duplicate target" => {
                database.execute(&format!("INSERT INTO package_requirement_groups (trove_id, kind, version_scheme, requirement_json) SELECT trove_id, kind, version_scheme, json_set(requirement_json, '$.native_text', 'duplicate') FROM package_requirement_groups {target}"), []).unwrap();
            }
            "duplicate same-name trove" => insert_trove(&database, 3, NAME, "9.9.9-1", lane),
            "wrong trove identity" => {
                database
                    .execute(
                        "UPDATE troves SET install_reason = 'dependency' WHERE id = 1",
                        [],
                    )
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let actual = execute_json_query(&database, query).expect(defect);
        rejects(assertion, &actual, defect);
    }
    let database = fixture_database(lane);
    database
        .execute(
            &format!(
                "UPDATE package_requirement_groups SET requirement_json = '{{broken' {target}"
            ),
            [],
        )
        .unwrap();
    assert!(
        execute_json_query(&database, query).is_err(),
        "malformed persisted JSON must fail SQLite"
    );
}

fn legacy_assertion(lane: &Lane) -> Assertion {
    Assertion {
        exit_code: Some(0),
        stdout_contains_all: Some(vec![
            format!("{} requirement groups", lane.count),
            TARGET.to_owned(),
            format!("{TARGET}|= 1.0.0"),
        ]),
        ..Assertion::default()
    }
}

fn legacy_stdout(database: &Connection) -> String {
    let count: String = database.query_row("SELECT COUNT(*) || ' requirement groups' FROM package_requirement_groups WHERE trove_id = 1", [], |row| row.get(0)).unwrap();
    let groups: String = database.query_row("SELECT group_concat(kind || '|' || version_scheme || '|' || requirement_json, char(10)) FROM package_requirement_groups WHERE trove_id = 1", [], |row| row.get(0)).unwrap();
    let target: String = database.query_row("SELECT json_extract(requirement_json, '$.expression.operands.name') || '|' || json_extract(requirement_json, '$.expression.operands.version_constraint') FROM package_requirement_groups WHERE trove_id = 1 AND json_extract(requirement_json, '$.expression.operands.name') = 'phase4-repository-fixture'", [], |row| row.get(0)).unwrap();
    format!("{count}\n{groups}\n{target}\n")
}

fn reject_bad_output(assertion: &Assertion, expected: &Value) {
    rejects(assertion, &json!([]), "missing output row");
    let mut duplicate = expected.clone();
    duplicate.as_array_mut().unwrap().push(expected[0].clone());
    rejects(assertion, &duplicate, "duplicate output row");
    let mut extra = expected.clone();
    extra[0]["unexpected"] = json!(true);
    rejects(assertion, &extra, "extra field");
    let mut missing = expected.clone();
    missing[0]
        .as_object_mut()
        .unwrap()
        .remove("expression_name");
    rejects(assertion, &missing, "missing field");
    let mut null = expected.clone();
    null[0]["expression_name"] = Value::Null;
    rejects(assertion, &null, "null field");
    let mut wrong_type = expected.clone();
    wrong_type[0]["alternatives_count"] = json!("1");
    rejects(assertion, &wrong_type, "wrong field type");
    let valid = expected.to_string();
    for stdout in [
        "not JSON",
        &format!("{valid} trailing"),
        &format!("{valid}{valid}"),
    ] {
        assert!(
            evaluate_assertion(assertion, 0, stdout, "").is_err(),
            "bad output: {stdout}"
        );
    }
    assert!(
        evaluate_assertion(assertion, 1, &valid, "").is_err(),
        "sqlite failure"
    );
}

fn rejects(assertion: &Assertion, rows: &Value, defect: &str) {
    assert!(
        evaluate_assertion(assertion, 0, &rows.to_string(), "").is_err(),
        "must reject {defect}: {rows}"
    );
}
