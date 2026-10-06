// crates/conary-core/src/source_root/tests/pin_storage.rs

use super::super::pin::{StoredPin, initialize_pinned_database, read_pin};
use super::{fresh_registry, name};
use rusqlite::ffi;

fn constraint_code(error: rusqlite::Error) -> i32 {
    match error {
        rusqlite::Error::SqliteFailure(failure, _) => failure.extended_code,
        other => panic!("expected an SQLite constraint failure, got {other:?}"),
    }
}

#[test]
fn host_database_carries_no_pin() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("conary.db");
    crate::db::init(&db_path).unwrap();
    let conn = crate::db::open(&db_path).unwrap();

    assert_eq!(read_pin(&conn).unwrap(), StoredPin::Missing);
}

#[test]
fn pin_is_written_with_the_schema_and_is_immutable() {
    let (_temp, registry) = fresh_registry();
    let root = registry.create(&name("arch")).unwrap();
    let conn = crate::db::open(root.db_path()).unwrap();
    let StoredPin::Present { identity, .. } = read_pin(&conn).unwrap() else {
        panic!("created root must carry a pin");
    };
    assert_eq!(identity, name("arch"));

    let update = conn
        .execute("UPDATE source_root_identity SET identity = 'fedora-44'", [])
        .unwrap_err();
    assert_eq!(constraint_code(update), ffi::SQLITE_CONSTRAINT_TRIGGER);

    let delete = conn
        .execute("DELETE FROM source_root_identity", [])
        .unwrap_err();
    assert_eq!(constraint_code(delete), ffi::SQLITE_CONSTRAINT_TRIGGER);

    let second = conn
        .execute(
            "INSERT INTO source_root_identity (singleton, identity) VALUES (1, 'fedora-44')",
            [],
        )
        .unwrap_err();
    assert_eq!(constraint_code(second), ffi::SQLITE_CONSTRAINT_PRIMARYKEY);

    let other_singleton = conn
        .execute(
            "INSERT INTO source_root_identity (singleton, identity) VALUES (2, 'fedora-44')",
            [],
        )
        .unwrap_err();
    assert_eq!(
        constraint_code(other_singleton),
        ffi::SQLITE_CONSTRAINT_CHECK
    );
    drop(conn);

    // Positive control: the refused writes left the pin and the root intact.
    assert_eq!(registry.open(&name("arch")).unwrap(), root);
}

#[test]
fn pin_identity_is_constrained_on_write() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("conary.db");
    crate::db::init(&db_path).unwrap();
    let conn = crate::db::open(&db_path).unwrap();

    for invalid in ["", "Arch", ".hidden", "a/b", "a b"] {
        let error = conn
            .execute(
                "INSERT INTO source_root_identity (singleton, identity) VALUES (1, ?1)",
                [invalid],
            )
            .unwrap_err();
        assert_eq!(
            constraint_code(error),
            ffi::SQLITE_CONSTRAINT_CHECK,
            "identity {invalid:?}"
        );
    }
    let too_long = "a".repeat(65);
    let error = conn
        .execute(
            "INSERT INTO source_root_identity (singleton, identity) VALUES (1, ?1)",
            [too_long.as_str()],
        )
        .unwrap_err();
    assert_eq!(constraint_code(error), ffi::SQLITE_CONSTRAINT_CHECK);

    // Positive control: a grammatical identity is accepted by the same table.
    conn.execute(
        "INSERT INTO source_root_identity (singleton, identity) VALUES (1, 'ubuntu-26.04')",
        [],
    )
    .unwrap();
    assert!(matches!(
        read_pin(&conn).unwrap(),
        StoredPin::Present { identity, .. } if identity == name("ubuntu-26.04")
    ));
}

#[test]
fn pinned_initialization_refuses_an_existing_database() {
    let temp = tempfile::tempdir().unwrap();
    let existing = temp.path().join("existing.db");
    crate::db::init(&existing).unwrap();

    let error = initialize_pinned_database(&existing, &name("arch")).unwrap_err();
    assert!(matches!(error, crate::Error::InitError(_)), "{error:?}");
    let conn = crate::db::open(&existing).unwrap();
    assert_eq!(read_pin(&conn).unwrap(), StoredPin::Missing);

    // Positive control: a fresh path is initialized with the pin.
    let fresh = temp.path().join("fresh.db");
    initialize_pinned_database(&fresh, &name("arch")).unwrap();
    let conn = crate::db::open(&fresh).unwrap();
    assert!(matches!(
        read_pin(&conn).unwrap(),
        StoredPin::Present { identity, .. } if identity == name("arch")
    ));
}
