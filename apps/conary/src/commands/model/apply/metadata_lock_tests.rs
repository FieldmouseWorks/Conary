// apps/conary/src/commands/model/apply/metadata_lock_tests.rs

//! Metadata apply selects installed state on the far side of the runtime
//! mutation lock.
//!
//! Pin, unpin, and install-reason changes are read-modify-write operations on
//! one installed trove. Selecting that trove before the lock is held lets a
//! concurrent transaction delete or change it between selection and write.
//! These tests hold the lock, prove the apply is blocked, mutate or delete the
//! selected record through an owner connection, then release and assert the
//! exact outcome the re-read state earns.

use std::time::Duration;

use conary_core::db::models::{InstallReason, InstallSource, Trove, TroveType};
use conary_core::model::DiffAction;
use conary_core::repository::versioning::VersionScheme;

use super::apply_metadata_changes;
use crate::commands::generation::selected_root::LockedRuntimeRoot;
use crate::commands::test_helpers::create_test_db;

const BLOCKED_PROOF_WAIT: Duration = Duration::from_millis(250);
const RELEASE_WAIT: Duration = Duration::from_secs(30);

fn insert_trove(
    db_path: &str,
    name: &str,
    version: &str,
    release: &str,
    install_reason: InstallReason,
    pinned: bool,
) -> i64 {
    let conn = conary_core::db::open(db_path).unwrap();
    let mut trove = Trove::new_with_source(
        name.to_string(),
        version.to_string(),
        TroveType::Package,
        InstallSource::Repository,
        VersionScheme::Conary,
    );
    trove.architecture = Some("x86_64".to_string());
    trove.package_release = Some(release.to_string());
    trove.install_reason = install_reason;
    trove.selection_reason = Some("Explicitly installed".to_string());
    trove.pinned = pinned;
    trove.insert(&conn).unwrap()
}

fn stored_trove(db_path: &str, id: i64) -> Trove {
    let conn = conary_core::db::open(db_path).unwrap();
    Trove::find_by_id(&conn, id)
        .unwrap()
        .expect("metadata apply must not remove the selected trove")
}

/// Run `apply_metadata_changes` in a worker while this thread holds the
/// runtime mutation lock, prove the worker blocks, mutate installed state
/// under the lock, then release and return the worker's verdict.
fn apply_while_lock_is_held<R>(
    db_path: &str,
    actions: Vec<DiffAction>,
    under_lock: impl FnOnce() -> R,
) -> (usize, Vec<String>, R) {
    let locked = LockedRuntimeRoot::acquire(db_path).unwrap();
    let (attempt_tx, attempt_rx) = std::sync::mpsc::sync_channel(0);
    let (result_tx, result_rx) = std::sync::mpsc::sync_channel(0);
    let worker_db_path = db_path.to_string();
    let worker = std::thread::spawn(move || {
        let action_refs: Vec<&DiffAction> = actions.iter().collect();
        attempt_tx.send(()).unwrap();
        let outcome = apply_metadata_changes(&worker_db_path, &action_refs);
        result_tx.send(outcome).unwrap();
    });

    attempt_rx.recv().unwrap();
    assert!(
        matches!(
            result_rx.recv_timeout(BLOCKED_PROOF_WAIT),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ),
        "metadata apply reached a verdict while another transaction held the mutation lock"
    );

    let observed = under_lock();
    drop(locked);
    let (applied, errors) = result_rx
        .recv_timeout(RELEASE_WAIT)
        .expect("metadata apply must finish once the mutation lock is released");
    worker.join().unwrap();
    (applied, errors, observed)
}

#[test]
fn pin_applies_against_the_record_created_while_the_lock_was_held() {
    let (_temp, db_path) = create_test_db();
    let name = "pin-lock-fixture";
    let (applied, errors, id) = apply_while_lock_is_held(
        &db_path,
        vec![DiffAction::Pin {
            package: name.to_string(),
            pattern: "*".to_string(),
        }],
        || insert_trove(&db_path, name, "1.0.0", "1", InstallReason::Explicit, false),
    );

    assert_eq!(applied, 1);
    assert!(errors.is_empty(), "{errors:?}");
    let trove = stored_trove(&db_path, id);
    assert!(trove.pinned, "pin must persist on the re-read trove");
    assert_eq!(trove.install_reason, InstallReason::Explicit);
    assert_eq!(
        trove.selection_reason.as_deref(),
        Some("Explicitly installed")
    );
}

#[test]
fn unpin_applies_against_the_record_created_while_the_lock_was_held() {
    let (_temp, db_path) = create_test_db();
    let name = "unpin-lock-fixture";
    let (applied, errors, id) = apply_while_lock_is_held(
        &db_path,
        vec![DiffAction::Unpin {
            package: name.to_string(),
        }],
        || insert_trove(&db_path, name, "1.0.0", "1", InstallReason::Explicit, true),
    );

    assert_eq!(applied, 1);
    assert!(errors.is_empty(), "{errors:?}");
    let trove = stored_trove(&db_path, id);
    assert!(!trove.pinned, "unpin must persist on the re-read trove");
    assert_eq!(trove.install_reason, InstallReason::Explicit);
    assert_eq!(
        trove.selection_reason.as_deref(),
        Some("Explicitly installed")
    );
}

#[test]
fn mark_explicit_promotes_the_dependency_created_while_the_lock_was_held() {
    let (_temp, db_path) = create_test_db();
    let name = "mark-explicit-lock-fixture";
    let (applied, errors, id) = apply_while_lock_is_held(
        &db_path,
        vec![DiffAction::MarkExplicit {
            package: name.to_string(),
        }],
        || {
            insert_trove(
                &db_path,
                name,
                "1.0.0",
                "1",
                InstallReason::Dependency,
                false,
            )
        },
    );

    assert_eq!(applied, 1);
    assert!(errors.is_empty(), "{errors:?}");
    let trove = stored_trove(&db_path, id);
    assert_eq!(trove.install_reason, InstallReason::Explicit);
    assert_eq!(
        trove.selection_reason.as_deref(),
        Some("Marked explicit by model apply")
    );
    assert!(!trove.pinned);
}

#[test]
fn mark_dependency_demotes_the_explicit_trove_created_while_the_lock_was_held() {
    let (_temp, db_path) = create_test_db();
    let name = "mark-dependency-lock-fixture";
    let (applied, errors, id) = apply_while_lock_is_held(
        &db_path,
        vec![DiffAction::MarkDependency {
            package: name.to_string(),
        }],
        || insert_trove(&db_path, name, "1.0.0", "1", InstallReason::Explicit, false),
    );

    assert_eq!(applied, 1);
    assert!(errors.is_empty(), "{errors:?}");
    let trove = stored_trove(&db_path, id);
    assert_eq!(trove.install_reason, InstallReason::Dependency);
    assert_eq!(
        trove.selection_reason.as_deref(),
        Some("Explicitly installed")
    );
    assert!(!trove.pinned);
}

#[test]
fn metadata_actions_re_read_target_existence_after_the_lock_is_released() {
    let name = "gone-lock-fixture";
    let cases = [
        (
            DiffAction::Pin {
                package: name.to_string(),
                pattern: "*".to_string(),
            },
            InstallReason::Explicit,
            Some(format!("Pin '{name}': Package '{name}' is not installed")),
        ),
        (
            DiffAction::Unpin {
                package: name.to_string(),
            },
            InstallReason::Explicit,
            Some(format!("Unpin '{name}': Package '{name}' is not installed")),
        ),
        (
            DiffAction::MarkExplicit {
                package: name.to_string(),
            },
            InstallReason::Dependency,
            Some(format!(
                "MarkExplicit '{name}': Package '{name}' is not installed"
            )),
        ),
        (
            DiffAction::MarkDependency {
                package: name.to_string(),
            },
            InstallReason::Explicit,
            Some(format!(
                "MarkDependency '{name}': Package '{name}' is not installed"
            )),
        ),
    ];

    for (action, install_reason, expected_error) in cases {
        let (_temp, db_path) = create_test_db();
        insert_trove(&db_path, name, "1.0.0", "1", install_reason, false);
        let (applied, errors, deleted) = apply_while_lock_is_held(&db_path, vec![action], || {
            let conn = conary_core::db::open(&db_path).unwrap();
            conn.execute("DELETE FROM troves WHERE name = ?1", [name])
                .unwrap()
        });

        assert_eq!(deleted, 1, "the fixture must delete exactly one target");
        assert_eq!(applied, 0, "{errors:?}");
        match expected_error {
            Some(expected) => assert_eq!(errors, vec![expected]),
            None => assert!(errors.is_empty(), "{errors:?}"),
        }
        let conn = conary_core::db::open(&db_path).unwrap();
        assert!(
            Trove::find_by_name(&conn, name).unwrap().is_empty(),
            "a deleted target must not be recreated by metadata apply"
        );
    }
}

#[test]
fn ambiguous_metadata_target_is_refused_without_writes() {
    let (_temp, db_path) = create_test_db();
    let name = "ambiguous-lock-fixture";
    let first = insert_trove(&db_path, name, "1.0.0", "1", InstallReason::Explicit, false);
    let (applied, errors, second) = apply_while_lock_is_held(
        &db_path,
        vec![DiffAction::Pin {
            package: name.to_string(),
            pattern: "*".to_string(),
        }],
        || insert_trove(&db_path, name, "1.0.0", "2", InstallReason::Explicit, false),
    );

    assert_eq!(applied, 0);
    assert_eq!(errors.len(), 1);
    assert!(
        errors[0].starts_with(&format!(
            "Pin '{name}': Multiple installed variants of '{name}'"
        )),
        "{errors:?}"
    );
    assert!(errors[0].contains("release 1"), "{errors:?}");
    assert!(errors[0].contains("release 2"), "{errors:?}");
    assert!(!errors[0].contains("--version"), "{errors:?}");
    for id in [first, second] {
        let trove = stored_trove(&db_path, id);
        assert!(
            !trove.pinned,
            "ambiguity refusal must not write either variant"
        );
        assert_eq!(trove.install_reason, InstallReason::Explicit);
    }
}

#[test]
fn non_metadata_actions_never_acquire_the_lock_or_open_the_database() {
    let temp = tempfile::tempdir().unwrap();
    let missing_db = temp.path().join("missing").join("conary.db");
    let missing_db_path = missing_db.to_string_lossy().into_owned();
    let install = DiffAction::Install {
        package: "noop".to_string(),
        pin: None,
        optional: false,
    };

    for actions in [Vec::new(), vec![&install]] {
        let outcome = apply_metadata_changes(&missing_db_path, &actions);
        assert_eq!(outcome, (0usize, Vec::<String>::new()));
    }
    assert!(
        !missing_db.parent().unwrap().exists(),
        "a request with no metadata actions must return before touching the runtime root"
    );
}
