// apps/conary/src/commands/update/pinning/tests.rs

//! Pin and unpin must select their target on the mutation lock's far side.
//!
//! Issue #976: both handlers selected and wrote pin state without acquiring the
//! runtime mutation lock. A release deleted in that window left the handler
//! holding a stale `trove_id`, so the `UPDATE` matched no row and the command
//! reported success against a package that no longer existed. These tests hold
//! the lock in the test thread, let each handler block in `acquire`, delete
//! only the selected release while it waits, then require a
//! not-installed error instead of a silent success.

use super::{cmd_pin, cmd_unpin};
use crate::commands::generation::selected_root::LockedRuntimeRoot;
use crate::commands::test_helpers::create_test_db;
use crate::commands::{InstalledPackageSelector, InstalledRelease};
use conary_core::db::models::{InstallSource, Trove, TroveType};
use conary_core::repository::versioning::VersionScheme;
use std::sync::mpsc::{RecvTimeoutError, sync_channel};
use std::time::Duration;

const PACKAGE: &str = "demo";
const VERSION: &str = "1.0.0";
const ARCH: &str = "x86_64";

fn insert_release(conn: &rusqlite::Connection, release: &str, pinned: bool) -> i64 {
    let mut trove = Trove::new_with_source(
        PACKAGE.to_string(),
        VERSION.to_string(),
        TroveType::Package,
        InstallSource::Repository,
        VersionScheme::Conary,
    );
    trove.architecture = Some(ARCH.to_string());
    trove.package_release = Some(release.to_string());
    trove.pinned = pinned;
    trove.insert(conn).unwrap()
}

fn seed_two_releases(db_path: &str, release_two_pinned: bool, release_one_pinned: bool) -> i64 {
    let conn = conary_core::db::open(db_path).unwrap();
    insert_release(&conn, "1", release_one_pinned);
    let release_two_id = insert_release(&conn, "2", release_two_pinned);
    let installed = Trove::find_by_name(&conn, PACKAGE).unwrap();
    assert_eq!(installed.len(), 2, "fixture must install both releases");
    release_two_id
}

fn release_two_selector() -> InstalledPackageSelector {
    InstalledPackageSelector::new(
        PACKAGE.to_string(),
        Some(VERSION.to_string()),
        Some(ARCH.to_string()),
    )
    .with_release(Some(InstalledRelease::Exact("2".to_string())))
}

fn assert_release_one_survives(db_path: &str, pinned: bool) {
    let conn = conary_core::db::open(db_path).unwrap();
    let installed = Trove::find_by_name(&conn, PACKAGE).unwrap();
    assert_eq!(
        installed.len(),
        1,
        "only the selected release 2 may be deleted"
    );
    let survivor = &installed[0];
    assert_eq!(survivor.package_release.as_deref(), Some("1"));
    assert_eq!(survivor.version, VERSION);
    assert_eq!(survivor.architecture.as_deref(), Some(ARCH));
    assert_eq!(survivor.pinned, pinned, "release 1 must survive unchanged");
}

/// Hold the mutation lock while starting `command`, optionally delete its
/// selected release, then verify the selected and sibling state after it runs.
fn release_two_changed_while_waiting(
    command: fn(InstalledPackageSelector, &str) -> anyhow::Result<()>,
    release_two_pinned: bool,
    release_one_pinned: bool,
    delete_target: bool,
) -> Result<(), String> {
    let (_temp, db_path) = create_test_db();
    let release_two_id = seed_two_releases(&db_path, release_two_pinned, release_one_pinned);

    let locked = LockedRuntimeRoot::acquire(&db_path).unwrap();

    let (attempt_tx, attempt_rx) = sync_channel(0);
    let (result_tx, result_rx) = sync_channel(0);
    let waiter_db_path = db_path.clone();
    let waiter = std::thread::spawn(move || {
        attempt_tx.send(()).unwrap();
        let result =
            command(release_two_selector(), &waiter_db_path).map_err(|error| format!("{error:#}"));
        result_tx.send(result).unwrap();
    });

    attempt_rx.recv().unwrap();
    assert!(
        matches!(
            result_rx.recv_timeout(Duration::from_millis(250)),
            Err(RecvTimeoutError::Timeout)
        ),
        "a handler reached a verdict while another holder owned the mutation lock"
    );

    // Change only the selected release while the handler waits.
    if delete_target {
        let conn = conary_core::db::open(&db_path).unwrap();
        Trove::delete(&conn, release_two_id).unwrap();
        assert_release_one_survives(&db_path, release_one_pinned);
    }
    drop(locked);

    let result = result_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("handler must reach a verdict once the mutation lock is released");
    waiter.join().unwrap();

    if delete_target {
        assert_release_one_survives(&db_path, release_one_pinned);
    } else {
        let conn = conary_core::db::open(&db_path).unwrap();
        let installed = Trove::find_by_name(&conn, PACKAGE).unwrap();
        assert_eq!(installed.len(), 2);
        let sibling = installed
            .iter()
            .find(|row| row.package_release.as_deref() == Some("1"))
            .unwrap();
        assert_eq!(sibling.pinned, release_one_pinned);
        let selected = Trove::find_by_id(&conn, release_two_id).unwrap().unwrap();
        assert_eq!(selected.package_release.as_deref(), Some("2"));
        assert_eq!(selected.pinned, !release_two_pinned);
    }
    result
}

#[test]
fn pin_blocks_and_reports_the_selected_release_deleted_while_waiting() {
    let error = release_two_changed_while_waiting(cmd_pin, false, false, true).unwrap_err();
    assert!(error.contains("is not installed"), "{error}");
    assert!(error.contains("release=2"), "{error}");
}

#[test]
fn unpin_blocks_and_reports_the_selected_release_deleted_while_waiting() {
    let error = release_two_changed_while_waiting(cmd_unpin, true, true, true).unwrap_err();
    assert!(error.contains("is not installed"), "{error}");
    assert!(error.contains("release=2"), "{error}");
}

#[test]
fn pin_and_unpin_change_only_the_selected_release_after_waiting() {
    release_two_changed_while_waiting(cmd_pin, false, false, false).unwrap();
    release_two_changed_while_waiting(cmd_unpin, true, true, false).unwrap();
}
