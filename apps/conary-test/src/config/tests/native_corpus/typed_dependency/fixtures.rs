// apps/conary-test/src/config/tests/native_corpus/typed_dependency/fixtures.rs
#![cfg(test)]

use super::{COLUMNS, LEGACY_QUERY, Lane, NAME, QUERY, reject};
use crate::config::manifest::Assertion;
use conary_core::{
    db::{
        self,
        models::{InstallReason, InstallSource, Trove, TroveType},
    },
    repository::dependency_model::DebianMultiArch,
    repository::versioning::VersionScheme,
};
use rusqlite::{Connection, params, types::ValueRef};
use serde_json::{Value, json};

pub(super) fn production_database(lane: &Lane) -> (tempfile::TempDir, Connection, i64, i64) {
    let directory = tempfile::tempdir().expect("create current-schema witness directory");
    let path = directory.path().join("state.db");
    db::init(&path).expect("initialize production schema");
    let database = db::open(&path).expect("open production database");
    database
        .execute(
            "INSERT INTO repositories (name, url) VALUES (?1, 'https://example.test/repo')",
            params![lane.repository],
        )
        .expect("insert fixture repository");
    let repository_id = database.last_insert_rowid();
    let target_id = insert_trove(&database, lane, NAME, "1.0.0", Some(repository_id));
    insert_trove(
        &database,
        lane,
        "unrelated-package",
        "8.0",
        Some(repository_id),
    );
    (directory, database, target_id, repository_id)
}

pub(super) fn insert_trove(
    database: &Connection,
    lane: &Lane,
    name: &str,
    version: &str,
    repository_id: Option<i64>,
) -> i64 {
    let mut trove = Trove::new(name.into(), version.into(), TroveType::Package, lane.scheme);
    trove.package_release = Some("1".into());
    trove.architecture = Some(lane.architecture.into());
    trove.source_profile = Some(lane.profile.into());
    trove.install_source = InstallSource::Repository;
    trove.install_reason = InstallReason::Dependency;
    trove.installed_from_repository_id = repository_id;
    if lane.scheme == VersionScheme::Debian {
        trove.debian_multi_arch = Some(DebianMultiArch::No);
    }
    trove.insert(database).expect("insert production trove")
}

pub(super) fn execute_json_query(database: &Connection, query: &str) -> Value {
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

pub(super) fn reject_persisted_defects(
    database: &Connection,
    target_id: i64,
    assertion: &Assertion,
) {
    for (column, bad) in [
        ("version", Some("9.9.9")),
        ("package_release", Some("2")),
        ("architecture", Some("wrong-arch")),
        ("source_profile", None),
        ("install_source", Some("file")),
        ("install_reason", Some("explicit")),
        ("installed_from_repository_id", None),
    ] {
        database.execute_batch("SAVEPOINT defect").unwrap();
        let sql = format!("UPDATE troves SET {column} = ?1 WHERE id = ?2");
        database
            .execute(&sql, params![bad, target_id])
            .expect("mutate persisted row");
        reject(assertion, &execute_json_query(database, QUERY), column);
        database
            .execute_batch("ROLLBACK TO defect; RELEASE defect")
            .unwrap();
    }
    database.execute_batch("SAVEPOINT defect").unwrap();
    database
        .execute(
            "UPDATE repositories SET name = 'wrong-repository' WHERE id =
        (SELECT installed_from_repository_id FROM troves WHERE id = ?1)",
            params![target_id],
        )
        .expect("mutate joined repository");
    reject(
        assertion,
        &execute_json_query(database, QUERY),
        "repository join",
    );
    database
        .execute_batch("ROLLBACK TO defect; RELEASE defect")
        .unwrap();
    database.execute_batch("SAVEPOINT defect").unwrap();
    database
        .execute("DELETE FROM troves WHERE id = ?1", params![target_id])
        .unwrap();
    reject(
        assertion,
        &execute_json_query(database, QUERY),
        "missing dependency row",
    );
    database
        .execute_batch("ROLLBACK TO defect; RELEASE defect")
        .unwrap();
}

pub(super) fn legacy_stdout(database: &Connection) -> String {
    database
        .prepare(LEGACY_QUERY)
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
        .join("\n")
}
