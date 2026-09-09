// apps/conary/src/commands/install/batch/tests/replacement.rs

//! Replacement authority is revalidated under the mutation lock.
//!
//! A prepared `old_trove` is installed state read before the batch owned the
//! mutation lock. Another transaction can delete, alter, or duplicate that row
//! in the window before the batch locks. Validation must refuse before the
//! baseline generation is prepared or any payload is written, otherwise the
//! batch replaces a record it never certified.

use super::*;

const TARGET: &str = "replacement-target";
const TARGET_PATH: &str = "/usr/bin/replacement-target";
const OLD_PAYLOAD: &[u8] = b"old-replacement-payload";
const NEW_PAYLOAD: &[u8] = b"new-replacement-payload-longer";

/// Counts that must not move when replacement validation refuses: transaction
/// records, baseline selected-root snapshots, generation publications, and any
/// file row carrying the incoming payload's content.
fn authority_counts(conn: &rusqlite::Connection) -> (i64, i64, i64, i64) {
    let count = |table: &str| {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap()
    };
    let incoming_payload = conary_core::hash::sha256(NEW_PAYLOAD);
    let payload_rows = conn
        .query_row(
            "SELECT COUNT(*) FROM files WHERE content_sha256 = ?1",
            [incoming_payload],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    (
        count("changesets"),
        count("selected_root_snapshots"),
        count("generation_publications"),
        payload_rows,
    )
}

/// Race a prepared replacement against `mutate`, which runs while the batch is
/// blocked on the mutation lock. Returns the refusal message.
fn refused_replacement(mutate: impl FnOnce(&rusqlite::Connection)) -> String {
    let _mount_guard = crate::commands::composefs_ops::test_mount_skip_guard();
    let temp = tempfile::tempdir().unwrap();
    let (db_path, db_path_string) = db_with_prior_transaction(
        temp.path(),
        vec![prepared_test_package(TARGET, TARGET_PATH, OLD_PAYLOAD)],
    );
    let conn = conary_core::db::open(&db_path).unwrap();
    let old_trove = Trove::find_by_name(&conn, TARGET).unwrap().remove(0);
    let before = authority_counts(&conn);
    drop(conn);

    let mut incoming = prepared_test_package(TARGET, TARGET_PATH, NEW_PAYLOAD);
    incoming.version = "2.0.0".to_string();
    incoming.is_upgrade = true;
    incoming.old_trove = Some(Box::new(old_trove));

    let locked =
        crate::commands::generation::selected_root::LockedRuntimeRoot::acquire(&db_path_string)
            .unwrap();
    let (attempt_tx, attempt_rx) = std::sync::mpsc::sync_channel(0);
    let (result_tx, result_rx) = std::sync::mpsc::sync_channel(0);
    let waiter_db_path = db_path_string.clone();
    let waiter = std::thread::spawn(move || {
        attempt_tx.send(()).unwrap();
        let result = BatchInstaller::new(&waiter_db_path, SandboxMode::Always)
            .install_batch(vec![incoming])
            .map_err(|error| format!("{error:#}"));
        result_tx.send(result).unwrap();
    });

    attempt_rx.recv().unwrap();
    assert!(
        matches!(
            result_rx.recv_timeout(std::time::Duration::from_millis(250)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ),
        "the replacement reached a verdict while another transaction held the mutation lock"
    );

    let conn = conary_core::db::open(&db_path).unwrap();
    mutate(&conn);
    drop(conn);
    drop(locked);

    let error = result_rx
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("the batch must reach a verdict once the mutation lock is released")
        .expect_err("a replacement committed against installed state mutated before it locked");
    waiter.join().unwrap();

    let conn = conary_core::db::open(&db_path).unwrap();
    assert_eq!(
        authority_counts(&conn),
        before,
        "replacement refusal must precede baseline generation and payload mutation"
    );
    error
}

#[test]
fn replacement_refuses_when_the_selected_row_disappeared_before_the_lock() {
    let error = refused_replacement(|conn| {
        assert_eq!(
            conn.execute("DELETE FROM troves WHERE name = ?1", [TARGET])
                .unwrap(),
            1
        );
    });
    assert!(error.contains("disappeared before replacement"), "{error}");
}

#[test]
fn replacement_refuses_when_the_selected_row_changed_before_the_lock() {
    let error = refused_replacement(|conn| {
        assert_eq!(
            conn.execute(
                "UPDATE troves SET version = '9.9.9' WHERE name = ?1",
                [TARGET]
            )
            .unwrap(),
            1
        );
    });
    assert!(error.contains("changed after selection"), "{error}");
}

#[test]
fn replacement_refuses_when_the_incoming_identity_already_lives_on_another_row() {
    let error = refused_replacement(|conn| {
        let mut duplicate = Trove::new(
            TARGET.to_string(),
            "2.0.0".to_string(),
            TroveType::Package,
            conary_core::repository::versioning::VersionScheme::Rpm,
        );
        duplicate.architecture = Some("x86_64".to_string());
        duplicate.insert(conn).unwrap();
    });
    assert!(
        error.contains("is already installed as a separate record"),
        "{error}"
    );
}

#[test]
fn planned_absent_target_is_rechecked_after_the_batch_waits_for_its_lock() {
    let _mount_guard = crate::commands::composefs_ops::test_mount_skip_guard();
    let temp = tempfile::tempdir().unwrap();
    let (db_path, db) = db_with_prior_transaction(
        temp.path(),
        vec![prepared_test_package(TARGET, TARGET_PATH, OLD_PAYLOAD)],
    );
    let conn = conary_core::db::open(&db_path).unwrap();
    let original = Trove::find_by_name(&conn, TARGET).unwrap().remove(0);
    let original_id = original.id.unwrap();
    conn.execute("DELETE FROM troves WHERE id = ?1", [original_id])
        .unwrap();
    let before = authority_counts(&conn);
    let mut incoming = prepared_test_package(TARGET, TARGET_PATH, NEW_PAYLOAD);
    incoming.version = "2.0.0".into();
    incoming.replacement = Some(crate::commands::install::InstallReplacement::PlannedAbsent(
        original.clone(),
    ));
    let locked =
        crate::commands::generation::selected_root::LockedRuntimeRoot::acquire(&db).unwrap();
    let (started_tx, started_rx) = std::sync::mpsc::sync_channel(0);
    let (result_tx, result_rx) = std::sync::mpsc::sync_channel(0);
    let waiter = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        let result = BatchInstaller::new(&db, SandboxMode::Always)
            .install_batch(vec![incoming])
            .map_err(|error| format!("{error:#}"));
        result_tx.send(result).unwrap();
    });
    started_rx.recv().unwrap();
    assert!(matches!(
        result_rx.recv_timeout(std::time::Duration::from_millis(250)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    ));
    let mut restored = original;
    let inserted = restored.insert(&conn).unwrap();
    conn.execute(
        "UPDATE troves SET id = ?1 WHERE id = ?2",
        [original_id, inserted],
    )
    .unwrap();
    drop(locked);
    let error = result_rx
        .recv_timeout(std::time::Duration::from_secs(60))
        .unwrap()
        .unwrap_err();
    waiter.join().unwrap();
    assert!(
        error.contains("was planned absent but is currently installed"),
        "{error}"
    );
    assert_eq!(authority_counts(&conn), before);
}
