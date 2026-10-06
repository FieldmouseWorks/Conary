// apps/conary-test/src/config/tests/native_corpus/typed_update_config/fixtures.rs
#![cfg(test)]

use super::{CONFIG_PATH, HASH, Lane, NAME, QUERY, REPOSITORY};
use crate::config::manifest::Assertion;
use crate::engine::assertions::evaluate_assertion;
use conary_core::{
    db::{
        self,
        models::{ConfigFile, ConfigSource, InstallReason, InstallSource, Trove, TroveType},
    },
    repository::{dependency_model::DebianMultiArch, versioning::VersionScheme},
};
use rusqlite::{Connection, params, types::ValueRef};
use serde_json::{Value, json};

const LEGACY_TROVE_QUERY: &str = "SELECT t.name || '|' || t.version || '|' || COALESCE(t.architecture, '') || '|' || t.version_scheme || '|' || COALESCE(t.source_profile, '') || '|' || COALESCE(r.name, '') || '|' || t.install_source || '|' || t.install_reason FROM troves t LEFT JOIN repositories r ON r.id = t.installed_from_repository_id WHERE t.name = 'phase4-daily-driver-corpus'";
const LEGACY_CONFIG_QUERY: &str = "SELECT path || '|' || original_hash || '|' || COALESCE(current_hash, '') || '|' || noreplace || '|' || status || '|' || source FROM config_files WHERE trove_id = (SELECT id FROM troves WHERE name = 'phase4-daily-driver-corpus') AND path = '/etc/phase4-corpus/app.conf'";

pub(super) fn production_database(lane: &Lane) -> (tempfile::TempDir, Connection, i64, i64, i64) {
    let directory = tempfile::tempdir().expect("create current-schema witness directory");
    let path = directory.path().join("state.db");
    db::init(&path).expect("initialize production schema");
    let database = db::open(&path).expect("open production database");
    database
        .execute(
            "INSERT INTO repositories (name, url) VALUES (?1, 'https://example.test/repo')",
            [REPOSITORY],
        )
        .expect("insert fixture repository");
    let repository_id = database.last_insert_rowid();
    let target_id = insert_trove(&database, lane, NAME, "1.0.1-1", repository_id);
    for path in [
        "/etc/phase4-corpus/app-deleted.conf",
        "/etc/phase4-corpus/app-local.conf",
        CONFIG_PATH,
        "/etc/phase4-corpus/app-unmatched.conf",
    ] {
        insert_config(&database, target_id, lane.source, path);
    }
    let other_id = insert_trove(&database, lane, "unrelated-package", "8.0", repository_id);
    insert_config(
        &database,
        other_id,
        lane.source,
        "/etc/unrelated-package/app.conf",
    );
    (directory, database, target_id, other_id, repository_id)
}

pub(super) fn insert_trove(
    database: &Connection,
    lane: &Lane,
    name: &str,
    version: &str,
    repository_id: i64,
) -> i64 {
    let mut trove = Trove::new(name.into(), version.into(), TroveType::Package, lane.scheme);
    trove.architecture = Some(lane.architecture.into());
    trove.source_profile = Some(lane.profile.into());
    trove.install_source = InstallSource::Repository;
    trove.install_reason = InstallReason::Explicit;
    trove.installed_from_repository_id = Some(repository_id);
    if lane.scheme == VersionScheme::Debian {
        trove.debian_multi_arch = Some(DebianMultiArch::No);
    }
    trove.insert(database).expect("insert production trove")
}

pub(super) fn insert_config(
    database: &Connection,
    trove_id: i64,
    source: ConfigSource,
    path: &str,
) {
    let mut config = ConfigFile::new_noreplace(path.into(), trove_id, HASH.into());
    config.source = source;
    config
        .insert(database)
        .expect("insert production config row");
}

pub(super) fn execute_json_query(database: &Connection, query: &str) -> Value {
    assert_eq!(query, QUERY);
    let mut statement = database
        .prepare(query)
        .expect("prepare loaded manifest SQL");
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
                    ValueRef::Text(value) => {
                        json!(std::str::from_utf8(value).expect("SQLite text"))
                    }
                    ValueRef::Blob(_) => panic!("unexpected blob in {name}"),
                };
                object.insert(name.clone(), value);
            }
            Ok(Value::Object(object))
        })
        .expect("execute loaded manifest SQL")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("read loaded manifest rows");
    Value::Array(rows)
}

pub(super) fn legacy_stdout(database: &Connection) -> String {
    let mut rows = Vec::new();
    for query in [LEGACY_TROVE_QUERY, LEGACY_CONFIG_QUERY] {
        rows.extend(
            database
                .prepare(query)
                .unwrap()
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap(),
        );
    }
    rows.join("\n")
}

pub(super) const COLUMNS: &[&str] = &[
    "trove_name",
    "trove_version",
    "trove_architecture",
    "trove_version_scheme",
    "trove_source_profile",
    "repository_name",
    "install_source",
    "install_reason",
    "config_path",
    "config_package_name",
    "config_package_version",
    "config_package_architecture",
    "original_hash",
    "current_hash",
    "noreplace",
    "status",
    "source",
    "config_owner_match",
    "related_config_rows",
];

pub(super) fn reject_persisted_defects(
    database: &rusqlite::Connection,
    target_id: i64,
    other_id: i64,
    assertion: &Assertion,
) {
    for (column, bad) in [
        ("package_name", "wrong-name"),
        ("package_version", "0.9.0"),
        ("package_architecture", "wrong-arch"),
        ("original_hash", "wrong-hash"),
        ("current_hash", "wrong-hash"),
        ("status", "modified"),
        ("source", "auto"),
    ] {
        database.execute_batch("SAVEPOINT defect").unwrap();
        database
            .execute(
                &format!("UPDATE config_files SET {column} = ?1 WHERE path = ?2"),
                params![bad, CONFIG_PATH],
            )
            .unwrap();
        reject(assertion, &execute_json_query(database, QUERY), column);
        database
            .execute_batch("ROLLBACK TO defect; RELEASE defect")
            .unwrap();
    }
    for (column, value) in [("trove_id", other_id), ("noreplace", 0)] {
        database.execute_batch("SAVEPOINT defect").unwrap();
        database
            .execute(
                &format!("UPDATE config_files SET {column} = ?1 WHERE path = ?2"),
                params![value, CONFIG_PATH],
            )
            .unwrap();
        reject(assertion, &execute_json_query(database, QUERY), column);
        database
            .execute_batch("ROLLBACK TO defect; RELEASE defect")
            .unwrap();
    }
    database.execute_batch("SAVEPOINT defect").unwrap();
    database
        .execute("DELETE FROM config_files WHERE path = ?1", [CONFIG_PATH])
        .unwrap();
    reject(
        assertion,
        &execute_json_query(database, QUERY),
        "missing app.conf",
    );
    database
        .execute_batch("ROLLBACK TO defect; RELEASE defect")
        .unwrap();
    for (column, bad) in [
        ("version", "0.9.0"),
        ("architecture", "wrong-arch"),
        ("install_source", "file"),
        ("install_reason", "dependency"),
    ] {
        database.execute_batch("SAVEPOINT defect").unwrap();
        database
            .execute(
                &format!("UPDATE troves SET {column} = ?1 WHERE id = ?2"),
                params![bad, target_id],
            )
            .unwrap();
        reject(assertion, &execute_json_query(database, QUERY), column);
        database
            .execute_batch("ROLLBACK TO defect; RELEASE defect")
            .unwrap();
    }
    database.execute_batch("SAVEPOINT defect").unwrap();
    database
        .execute(
            "UPDATE troves SET installed_from_repository_id = NULL WHERE id = ?1",
            [target_id],
        )
        .unwrap();
    reject(
        assertion,
        &execute_json_query(database, QUERY),
        "missing repository join",
    );
    database
        .execute_batch("ROLLBACK TO defect; RELEASE defect")
        .unwrap();
}

pub(super) fn assert_legacy_accepts(
    database: &rusqlite::Connection,
    lane: &Lane,
    assertion: &Assertion,
    actual: &Value,
) {
    let old = Assertion {
        exit_code: Some(0),
        stdout_contains_all: Some(vec![
            format!(
                "{NAME}|1.0.1-1|{}|{}|{}|{REPOSITORY}|repository|explicit",
                lane.architecture,
                lane.scheme.as_str(),
                lane.profile
            ),
            format!(
                "{CONFIG_PATH}|{HASH}|{HASH}|1|pristine|{}",
                lane.source.as_str()
            ),
        ]),
        ..Assertion::default()
    };
    assert!(
        evaluate_assertion(&old, 0, &legacy_stdout(database), "").is_ok(),
        "former substring assertion accepts the persisted defect"
    );
    reject(assertion, actual, "extra row or related config path");
}

pub(super) fn reject(assertion: &Assertion, actual: &Value, defect: &str) {
    assert!(
        evaluate_assertion(assertion, 0, &actual.to_string(), "").is_err(),
        "must reject {defect}"
    );
}
