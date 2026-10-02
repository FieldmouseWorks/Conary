// apps/conary-test/src/config/tests/native_corpus/typed_activation.rs
#![cfg(test)]

use super::super::{load_manifest, remi_manifest_path};
use crate::config::manifest::{Assertion, JsonExpectation, TestStep};
use crate::engine::{
    assertions::evaluate_assertion,
    variables::{expand_assertion, expand_variables},
};
use conary_core::{
    activation::{
        ActivationExecutableIdentity, BootRuntimeActivationInvocation,
        parse_systemctl_activation_invocation,
    },
    db::{
        models::{
            ActivationRequest, ActivationRequestSourceKind, Changeset, ChangesetStatus,
            GenerationActivationIntent, GenerationPublication, GenerationPublicationPhase,
            GenerationPublicationStatus, NewActivationRequest, SystemState, Trove, TroveType,
        },
        schema,
    },
    repository::{dependency_model::DebianMultiArch, versioning::VersionScheme},
};
use rusqlite::{Connection, params, types::ValueRef};
use serde_json::{Value, json};

const NAME: &str = "phase4-daily-driver-corpus";
const VERSION: &str = "1.0.0-1";
const GENERATION: i64 = 4;
const QUERY: &str = "SELECT t.name AS trove_name,t.version AS trove_version,r.source_kind,r.source_package,r.source_version,r.source_entry,json_extract(r.invocation_json,'$.kind') AS invocation_kind,json_extract(r.invocation_json,'$.invocation.action') AS systemd_action,json_array_length(r.invocation_json,'$.invocation.units') AS systemd_unit_count,json_extract(r.invocation_json,'$.invocation.units[0]') AS systemd_unit,json_extract(r.invocation_json,'$.invocation.program') AS boot_program,json_array_length(r.invocation_json,'$.invocation.arguments') AS boot_argument_count,json_extract(r.invocation_json,'$.invocation.arguments[0]') AS boot_argument,json_type(r.invocation_json,'$.invocation.schema_version') AS boot_schema_version_type,json_extract(r.invocation_json,'$.invocation.schema_version') AS boot_schema_version,json_extract(r.invocation_json,'$.invocation.executable.invoked_path') AS boot_invoked_path,gp.phase AS publication_phase,gp.status AS publication_status,(gp.published_through_changeset_id >= r.changeset_id) AS published_request,(gp.generation_number = i.generation_number) AS same_generation,i.status AS intent_status,i.attempt_count AS intent_attempt_count,i.last_error AS intent_last_error,i.started_at AS intent_started_at,i.completed_at AS intent_completed_at FROM troves AS t LEFT JOIN activation_requests AS r ON r.changeset_id=t.installed_by_changeset_id AND r.source_package=t.name LEFT JOIN generation_publications AS gp ON gp.trigger_changeset_id=t.installed_by_changeset_id AND gp.phase='database_backed_up' AND gp.status='complete' AND gp.recoverable=0 LEFT JOIN generation_activation_intents AS i ON i.request_id=r.id AND i.generation_number=gp.generation_number WHERE t.name='phase4-daily-driver-corpus' ORDER BY t.id,r.sequence,r.id,gp.id";
const COLUMNS: &[&str] = &[
    "trove_name",
    "trove_version",
    "source_kind",
    "source_package",
    "source_version",
    "source_entry",
    "invocation_kind",
    "systemd_action",
    "systemd_unit_count",
    "systemd_unit",
    "boot_program",
    "boot_argument_count",
    "boot_argument",
    "boot_schema_version_type",
    "boot_schema_version",
    "boot_invoked_path",
    "publication_phase",
    "publication_status",
    "published_request",
    "same_generation",
    "intent_status",
    "intent_attempt_count",
    "intent_last_error",
    "intent_started_at",
    "intent_completed_at",
];
const LANES: &[Lane] = &[
    Lane {
        distro: "fedora44",
        entry: "rpm:%post",
        arch: "x86_64",
        profile: "fedora-44",
        scheme: VersionScheme::Rpm,
    },
    Lane {
        distro: "ubuntu-26.04",
        entry: "deb:postinst",
        arch: "amd64",
        profile: "ubuntu-26.04",
        scheme: VersionScheme::Debian,
    },
    Lane {
        distro: "arch",
        entry: "arch:post_install",
        arch: "x86_64",
        profile: "arch",
        scheme: VersionScheme::Arch,
    },
];

struct Lane {
    distro: &'static str,
    entry: &'static str,
    arch: &'static str,
    profile: &'static str,
    scheme: VersionScheme,
}

struct Fixture {
    database: Connection,
    changeset_id: i64,
    target_requests: [i64; 2],
}

#[test]
fn native_corpus_tnpm15_activation_requires_exact_persisted_rows() {
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
    let step = &test.step[8];
    let command = step.run.as_deref().expect("step 9 SQL command");
    assert_eq!(command, format!("sqlite3 -json ${{DB_PATH}} \"{QUERY}\""));
    assert_eq!(command.matches("sqlite3").count(), 1);
    assert_eq!(command.matches("SELECT ").count(), 1);
    assert!(!QUERY.contains(';'), "step 9 runs one SQL statement");
    let query = sqlite_query(step);
    let assertion = root_assertion(step);

    for lane in LANES {
        let overrides = &manifest.distro_overrides[lane.distro];
        assert_eq!(
            overrides
                .get("native_corpus_fixture_version")
                .map(String::as_str),
            Some(VERSION)
        );
        assert_eq!(
            overrides
                .get("native_corpus_activation_entry")
                .map(String::as_str),
            Some(lane.entry)
        );
        assert_eq!(expand_variables(command, overrides), command);
        let expanded = expand_assertion(assertion, overrides);
        let expected = expected_rows(lane);
        assert_eq!(
            expanded.stdout_json.as_ref().unwrap()[0].expected,
            JsonExpectation::Equals(expected.clone()),
            "{} exact root JSON",
            lane.distro
        );

        let fixture = fixture_database(lane, false, "start");
        let actual = execute_json_query(&fixture.database, query);
        assert_eq!(actual, expected, "{} production-schema rows", lane.distro);
        assert!(evaluate_assertion(&expanded, 0, &actual.to_string(), "").is_ok());
        assert_eq!(
            GenerationActivationIntent::ready_for_generation(&fixture.database, GENERATION)
                .unwrap()
                .len(),
            3,
            "typed reader verifies three digests and invocations"
        );
        reject_persisted_defects(lane, query, &expanded, &expected);
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
    let assertion = step.assert.as_ref().expect("step 9 assertion");
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
    json!([
        {
            "trove_name": NAME, "trove_version": VERSION,
            "source_kind": "captured-systemctl", "source_package": NAME,
            "source_version": VERSION, "source_entry": lane.entry,
            "invocation_kind": "systemd", "systemd_action": "start",
            "systemd_unit_count": 1, "systemd_unit": "phase4-corpus.service",
            "boot_program": null, "boot_argument_count": null, "boot_argument": null,
            "boot_schema_version_type": null, "boot_schema_version": null,
            "boot_invoked_path": null, "publication_phase": "database_backed_up",
            "publication_status": "complete", "published_request": 1,
            "same_generation": 1, "intent_status": "pending",
            "intent_attempt_count": 0, "intent_last_error": null,
            "intent_started_at": null, "intent_completed_at": null,
        },
        {
            "trove_name": NAME, "trove_version": VERSION,
            "source_kind": "captured-boot-runtime", "source_package": NAME,
            "source_version": VERSION, "source_entry": lane.entry,
            "invocation_kind": "boot-runtime", "systemd_action": null,
            "systemd_unit_count": null, "systemd_unit": null,
            "boot_program": "depmod", "boot_argument_count": 1,
            "boot_argument": "-a", "boot_schema_version_type": "integer",
            "boot_schema_version": 1, "boot_invoked_path": "/usr/sbin/depmod",
            "publication_phase": "database_backed_up", "publication_status": "complete",
            "published_request": 1, "same_generation": 1,
            "intent_status": "pending", "intent_attempt_count": 0,
            "intent_last_error": null, "intent_started_at": null,
            "intent_completed_at": null,
        },
    ])
}

fn fixture_database(lane: &Lane, extra_target: bool, systemd_verb: &str) -> Fixture {
    let database = Connection::open_in_memory().expect("open disposable database");
    schema::ensure_current(&database).expect("initialize production schema");
    let mut changeset = Changeset::new("Install activation SQL fixture".into());
    let changeset_id = changeset
        .insert(&database)
        .expect("insert pending changeset");
    insert_trove(&database, lane, NAME, changeset_id);
    insert_trove(&database, lane, "phase4-repository-fixture", changeset_id);

    let mut requests = vec![
        systemd_request(
            "phase4-repository-fixture",
            lane.entry,
            "start",
            "foreign.service",
        ),
        systemd_request(NAME, lane.entry, systemd_verb, "phase4-corpus.service"),
        boot_request(lane.entry),
    ];
    if extra_target {
        requests.push(systemd_request(NAME, lane.entry, "start", "extra.service"));
    }
    let ids = ActivationRequest::append_batch(&database, changeset_id, &requests)
        .expect("persist typed activation requests while pending");
    changeset
        .update_status(&database, ChangesetStatus::Applied)
        .expect("apply changeset");
    SystemState::new(GENERATION, "activation fixture generation".into())
        .insert(&database)
        .expect("insert generation");
    assert_eq!(
        GenerationActivationIntent::project_through(&database, GENERATION, Some(changeset_id))
            .expect("project typed intents"),
        requests.len()
    );
    complete_publication(&database, changeset_id, GENERATION);
    Fixture {
        database,
        changeset_id,
        target_requests: [ids[1], ids[2]],
    }
}

fn insert_trove(database: &Connection, lane: &Lane, name: &str, changeset_id: i64) {
    let mut trove = Trove::new(name.into(), VERSION.into(), TroveType::Package, lane.scheme);
    trove.architecture = Some(lane.arch.into());
    trove.source_profile = Some(lane.profile.into());
    trove.installed_by_changeset_id = Some(changeset_id);
    if lane.scheme == VersionScheme::Debian {
        trove.debian_multi_arch = Some(DebianMultiArch::No);
    }
    trove.insert(database).expect("insert production trove");
}

fn systemd_request(package: &str, entry: &str, verb: &str, unit: &str) -> NewActivationRequest {
    let invocation = parse_systemctl_activation_invocation(&[verb.into(), unit.into()])
        .expect("parse typed systemctl invocation")
        .expect("runtime activation verb");
    NewActivationRequest {
        source_kind: ActivationRequestSourceKind::CapturedSystemctl,
        source_package: package.into(),
        source_version: VERSION.into(),
        source_entry: entry.into(),
        invocation: invocation.into(),
    }
}

fn boot_request(entry: &str) -> NewActivationRequest {
    let executable = ActivationExecutableIdentity {
        invoked_path: "/usr/sbin/depmod".into(),
        canonical_path: "/usr/sbin/depmod".into(),
        sha256: format!("sha256:{}", "a".repeat(64)),
    };
    NewActivationRequest {
        source_kind: ActivationRequestSourceKind::CapturedBootRuntime,
        source_package: NAME.into(),
        source_version: VERSION.into(),
        source_entry: entry.into(),
        invocation: BootRuntimeActivationInvocation::new("depmod", vec!["-a".into()], executable)
            .expect("typed depmod mutation")
            .into(),
    }
}

fn complete_publication(database: &Connection, changeset_id: i64, generation: i64) {
    let publication = GenerationPublication::create_pending(
        database,
        Some(changeset_id),
        None,
        "/tmp/activation.db",
        "/tmp/activation",
        "typed activation fixture",
        &Default::default(),
    )
    .expect("record selected-root publication");
    publication
        .set_phase(
            database,
            GenerationPublicationPhase::DatabaseBackedUp,
            GenerationPublicationStatus::Running,
            Some(generation),
            Some(generation),
        )
        .expect("reach durable backup phase");
    assert_eq!(
        publication
            .mark_complete_through(database, Some(changeset_id), generation, generation)
            .expect("complete exact publication"),
        1
    );
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

fn reject_persisted_defects(lane: &Lane, query: &str, assertion: &Assertion, expected: &Value) {
    let fixture = fixture_database(lane, false, "start");
    fixture
        .database
        .execute(
            "UPDATE activation_requests SET source_version='9.9' WHERE id IN (?1,?2)",
            params![fixture.target_requests[0], fixture.target_requests[1]],
        )
        .unwrap();
    assert_legacy_passes(&fixture.database);
    rejects(
        assertion,
        &execute_json_query(&fixture.database, query),
        "wrong source version",
    );

    let fixture = fixture_database(lane, false, "start");
    fixture.database.execute("DELETE FROM generation_activation_intents WHERE generation_number=?1 AND request_id IN (?2,?3)", params![GENERATION, fixture.target_requests[0], fixture.target_requests[1]]).unwrap();
    SystemState::new(GENERATION + 1, "later generation".into())
        .insert(&fixture.database)
        .unwrap();
    GenerationActivationIntent::project_through(
        &fixture.database,
        GENERATION + 1,
        Some(fixture.changeset_id),
    )
    .unwrap();
    assert_legacy_passes(&fixture.database);
    rejects(
        assertion,
        &execute_json_query(&fixture.database, query),
        "pending intents belong to another generation",
    );

    let fixture = fixture_database(lane, false, "start");
    fixture
        .database
        .execute(
            "DELETE FROM activation_requests WHERE id=?1",
            [fixture.target_requests[1]],
        )
        .unwrap();
    rejects(
        assertion,
        &execute_json_query(&fixture.database, query),
        "missing request",
    );

    let fixture = fixture_database(lane, false, "start");
    fixture.database.execute("DELETE FROM generation_activation_intents WHERE generation_number=?1 AND request_id=?2", params![GENERATION, fixture.target_requests[0]]).unwrap();
    rejects(
        assertion,
        &execute_json_query(&fixture.database, query),
        "missing intent",
    );

    let fixture = fixture_database(lane, true, "start");
    rejects(
        assertion,
        &execute_json_query(&fixture.database, query),
        "extra target request",
    );

    let fixture = fixture_database(lane, false, "restart");
    rejects(
        assertion,
        &execute_json_query(&fixture.database, query),
        "wrong typed invocation",
    );

    for (mutation, defect) in [
        (
            "UPDATE generation_activation_intents SET status='failed' WHERE generation_number=?1 AND request_id=?2",
            "wrong status",
        ),
        (
            "UPDATE generation_activation_intents SET attempt_count=1 WHERE generation_number=?1 AND request_id=?2",
            "wrong attempt count",
        ),
    ] {
        let fixture = fixture_database(lane, false, "start");
        fixture
            .database
            .execute(mutation, params![GENERATION, fixture.target_requests[0]])
            .unwrap();
        rejects(
            assertion,
            &execute_json_query(&fixture.database, query),
            defect,
        );
    }

    let fixture = fixture_database(lane, false, "start");
    fixture
        .database
        .execute(
            "UPDATE activation_requests SET source_entry='wrong-slot' WHERE id=?1",
            [fixture.target_requests[0]],
        )
        .unwrap();
    rejects(
        assertion,
        &execute_json_query(&fixture.database, query),
        "wrong native slot",
    );

    let fixture = fixture_database(lane, false, "start");
    fixture.database.execute("UPDATE generation_publications SET published_through_changeset_id=NULL WHERE trigger_changeset_id=?1", [fixture.changeset_id]).unwrap();
    rejects(
        assertion,
        &execute_json_query(&fixture.database, query),
        "publication misses request high-water",
    );

    let fixture = fixture_database(lane, false, "start");
    fixture
        .database
        .execute(
            "UPDATE generation_publications SET status='failed' WHERE trigger_changeset_id=?1",
            [fixture.changeset_id],
        )
        .unwrap();
    rejects(
        assertion,
        &execute_json_query(&fixture.database, query),
        "missing terminal publication",
    );

    // A same-named later request and its generation are legitimate foreign rows.
    let fixture = fixture_database(lane, false, "start");
    let mut later = Changeset::new("Unrelated later activation".into());
    let later_id = later.insert(&fixture.database).unwrap();
    ActivationRequest::append_batch(
        &fixture.database,
        later_id,
        &[systemd_request(NAME, lane.entry, "start", "later.service")],
    )
    .unwrap();
    later
        .update_status(&fixture.database, ChangesetStatus::Applied)
        .unwrap();
    SystemState::new(GENERATION + 1, "later generation".into())
        .insert(&fixture.database)
        .unwrap();
    GenerationActivationIntent::project_through(&fixture.database, GENERATION + 1, Some(later_id))
        .unwrap();
    complete_publication(&fixture.database, later_id, GENERATION + 1);
    assert_eq!(
        execute_json_query(&fixture.database, query),
        *expected,
        "foreign changeset and generation must not enter target rows"
    );
}

fn assert_legacy_passes(database: &Connection) {
    const REQUESTS: &str = "SELECT source_kind || '|' || source_package || '|' || json_extract(invocation_json, '$.kind') || '|' || COALESCE(json_extract(invocation_json, '$.invocation.action'), json_extract(invocation_json, '$.invocation.program')) || '|' || COALESCE(json_extract(invocation_json, '$.invocation.units[0]'), json_extract(invocation_json, '$.invocation.arguments[0]')) FROM activation_requests WHERE source_package = 'phase4-daily-driver-corpus' ORDER BY id";
    const INTENTS: &str = "SELECT status || '|' || COUNT(*) FROM generation_activation_intents WHERE request_id IN (SELECT id FROM activation_requests WHERE source_package = 'phase4-daily-driver-corpus') GROUP BY status";
    let mut lines = Vec::new();
    for query in [REQUESTS, INTENTS] {
        lines.extend(
            database
                .prepare(query)
                .unwrap()
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap(),
        );
    }
    let old = Assertion {
        exit_code: Some(0),
        stdout_contains_all: Some(vec![
            "captured-systemctl|phase4-daily-driver-corpus|systemd|start|phase4-corpus.service"
                .into(),
            "captured-boot-runtime|phase4-daily-driver-corpus|boot-runtime|depmod|-a".into(),
            "pending|2".into(),
        ]),
        ..Assertion::default()
    };
    assert!(
        evaluate_assertion(&old, 0, &lines.join("\n"), "").is_ok(),
        "legacy assertion still passes persisted defect"
    );
}

fn reject_bad_stdout(assertion: &Assertion, expected: &Value) {
    for actual in [
        json!([]),
        json!([expected[0].clone()]),
        json!([
            expected[0].clone(),
            expected[0].clone(),
            expected[1].clone()
        ]),
        json!({"rows": expected}),
    ] {
        rejects(assertion, &actual, "wrong JSON row shape or count");
    }
    let mut wrong_type = expected.clone();
    wrong_type[0]["same_generation"] = json!("1");
    rejects(assertion, &wrong_type, "wrong generation equality type");
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
