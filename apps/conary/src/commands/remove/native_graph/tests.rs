// apps/conary/src/commands/remove/native_graph/tests.rs

use super::*;
use conary_core::db::models::{InstallSource, TroveType};
use conary_core::repository::versioning::VersionScheme;

#[test]
fn remove_graph_resolves_package_identity_under_the_mutation_lock() {
    let _mount_skip = crate::commands::composefs_ops::test_mount_skip_guard();
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("conary.db");
    conary_core::db::init(&db_path).unwrap();
    crate::commands::test_helpers::seed_test_bootable_runtime(&db_path);

    let conn = conary_core::db::open(&db_path).unwrap();
    let mut trove = Trove::new_with_source(
        "remove-lock-fixture".to_string(),
        "1.0.0".to_string(),
        TroveType::Package,
        InstallSource::Repository,
        VersionScheme::Conary,
    );
    trove.architecture = Some("x86_64".to_string());
    let trove_id = trove.insert(&conn).unwrap();
    let trove = Trove::find_by_id(&conn, trove_id).unwrap().unwrap();
    drop(conn);

    let db_path_string = db_path.to_string_lossy().into_owned();
    let locked = LockedRuntimeRoot::acquire(&db_path_string).unwrap();
    let (attempt_tx, attempt_rx) = std::sync::mpsc::sync_channel(0);
    let (result_tx, result_rx) = std::sync::mpsc::sync_channel(0);
    let waiter_db_path = db_path_string.clone();
    let waiter = std::thread::spawn(move || {
        let conn = conary_core::db::open(&waiter_db_path).unwrap();
        let progress = RemoveProgress::new("remove-lock-fixture");
        attempt_tx.send(()).unwrap();
        let result = execute_installed_trove_remove_graph(
            &conn,
            &trove,
            &waiter_db_path,
            "remove-lock-fixture",
            RemoveLifecycleOptions::new(crate::commands::SandboxMode::Always),
            &progress,
        )
        .map_err(|error| format!("{error:#}"));
        result_tx.send(result).unwrap();
    });

    attempt_rx.recv().unwrap();
    assert!(
        matches!(
            result_rx.recv_timeout(std::time::Duration::from_millis(250)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ),
        "remove reached a verdict while another transaction held the mutation lock"
    );

    let conn = conary_core::db::open(&db_path).unwrap();
    assert_eq!(
        conn.execute(
            "UPDATE troves SET version = '2.0.0' WHERE id = ?1",
            [trove_id],
        )
        .unwrap(),
        1
    );
    drop(conn);
    drop(locked);

    let result = result_rx
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("remove must finish once the mutation lock is released");
    waiter.join().unwrap();
    let error = match result {
        Ok(_) => panic!("remove accepted package identity changed before it locked"),
        Err(error) => error,
    };
    assert!(
        error.contains("identity changed during removal preparation"),
        "{error}"
    );

    let conn = conary_core::db::open(&db_path).unwrap();
    assert_eq!(
        Trove::find_by_id(&conn, trove_id).unwrap().unwrap().version,
        "2.0.0"
    );
}

#[test]
fn remove_graph_refuses_package_pinned_while_waiting_for_mutation_lock() {
    let _mount_skip = crate::commands::composefs_ops::test_mount_skip_guard();
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("conary.db");
    conary_core::db::init(&db_path).unwrap();
    let bootable_trove_id = crate::commands::test_helpers::seed_test_bootable_runtime(&db_path);

    let conn = conary_core::db::open(&db_path).unwrap();
    let mut trove = Trove::new_with_source(
        "remove-post-lock-pin-fixture".to_string(),
        "1.0.0".to_string(),
        TroveType::Package,
        InstallSource::Repository,
        VersionScheme::Conary,
    );
    trove.architecture = Some("x86_64".to_string());
    let trove_id = trove.insert(&conn).unwrap();
    let trove = Trove::find_by_id(&conn, trove_id).unwrap().unwrap();
    assert!(
        !trove.pinned,
        "the selected snapshot handed to removal must start unpinned"
    );
    let mut sibling = Trove::new_with_source(
        "remove-post-lock-pin-sibling".to_string(),
        "3.0.0".to_string(),
        TroveType::Package,
        InstallSource::Repository,
        VersionScheme::Conary,
    );
    sibling.architecture = Some("x86_64".to_string());
    let sibling_id = sibling.insert(&conn).unwrap();
    let changesets_before: i64 = conn
        .query_row("SELECT COUNT(*) FROM changesets", [], |row| row.get(0))
        .unwrap();
    let publications_before: i64 = conn
        .query_row("SELECT COUNT(*) FROM generation_publications", [], |row| {
            row.get(0)
        })
        .unwrap();
    let snapshots_before: i64 = conn
        .query_row("SELECT COUNT(*) FROM selected_root_snapshots", [], |row| {
            row.get(0)
        })
        .unwrap();
    drop(conn);

    let db_path_string = db_path.to_string_lossy().into_owned();
    let locked = LockedRuntimeRoot::acquire(&db_path_string).unwrap();
    let (attempt_tx, attempt_rx) = std::sync::mpsc::sync_channel(0);
    let (result_tx, result_rx) = std::sync::mpsc::sync_channel(0);
    let waiter_db_path = db_path_string.clone();
    let waiter = std::thread::spawn(move || {
        let conn = conary_core::db::open(&waiter_db_path).unwrap();
        let progress = RemoveProgress::new("remove-post-lock-pin-fixture");
        attempt_tx.send(()).unwrap();
        let result = execute_installed_trove_remove_graph(
            &conn,
            &trove,
            &waiter_db_path,
            "remove-post-lock-pin-fixture",
            RemoveLifecycleOptions::new(crate::commands::SandboxMode::Always),
            &progress,
        )
        .map_err(|error| format!("{error:#}"));
        result_tx.send(result).unwrap();
    });

    attempt_rx.recv().unwrap();
    assert!(
        matches!(
            result_rx.recv_timeout(std::time::Duration::from_millis(250)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ),
        "remove reached a verdict while another transaction held the mutation lock"
    );

    let conn = conary_core::db::open(&db_path).unwrap();
    Trove::pin(&conn, trove_id).unwrap();
    assert!(
        Trove::find_by_id(&conn, trove_id).unwrap().unwrap().pinned,
        "fixture pin must land on the exact selected trove id"
    );
    drop(conn);
    drop(locked);

    let result = result_rx
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("remove must finish once the mutation lock is released");
    waiter.join().unwrap();
    let error = match result {
        Ok(_) => panic!("remove accepted a package pinned before it acquired the mutation lock"),
        Err(error) => error,
    };
    assert!(
        error.contains("is pinned and cannot be removed"),
        "post-lock pin must refuse removal through the graph guard, got: {error}"
    );

    let conn = conary_core::db::open(&db_path).unwrap();
    let selected = Trove::find_by_id(&conn, trove_id)
        .unwrap()
        .expect("refused removal must leave the selected trove installed");
    assert_eq!(selected.id, Some(trove_id));
    assert!(selected.pinned);
    assert_eq!(selected.version, "1.0.0");
    assert!(
        Trove::find_by_id(&conn, bootable_trove_id)
            .unwrap()
            .is_some(),
        "the bootable runtime fixture must survive the refused removal"
    );
    let sibling = Trove::find_by_id(&conn, sibling_id)
        .unwrap()
        .expect("unrelated sibling record must survive the refused removal");
    assert_eq!(sibling.id, Some(sibling_id));
    assert_eq!(sibling.name, "remove-post-lock-pin-sibling");
    assert_eq!(sibling.version, "3.0.0");
    assert!(!sibling.pinned);
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM changesets", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        changesets_before
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM generation_publications", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        publications_before
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM selected_root_snapshots", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        snapshots_before
    );
    drop(conn);

    let runtime_root = conary_core::runtime_root::ConaryRuntimeRoot::from_db_path(db_path.clone());
    let sessions = runtime_root.root().join("selected-root-sessions");
    assert!(
        !sessions.exists() || std::fs::read_dir(sessions).unwrap().next().is_none(),
        "a refusal before selected-root preparation must not leave a session behind"
    );
}

#[test]
#[cfg(feature = "test-hooks")]
fn removal_statistics_include_debian_config_purge() {
    use conary_core::ccs::native_lifecycle::{
        NATIVE_LIFECYCLE_SCHEMA_REVISION, NATIVE_LIFECYCLE_SCHEMA_V1, NativeLifecycleBundle,
        ScriptletFidelity, SourceFormat, VersionScheme as LifecycleVersionScheme,
    };
    use conary_core::db::models::{ConfigFile, ConfigSource, InstalledNativeLifecycleBundle};

    let _mount = crate::commands::composefs_ops::test_mount_skip_guard();
    let (_temp, db_path) = crate::commands::test_helpers::create_test_db();
    crate::commands::test_helpers::seed_test_bootable_runtime(std::path::Path::new(&db_path));
    let conn = conary_core::db::open(&db_path).unwrap();
    let mut trove = Trove::new(
        "purge-fixture".into(),
        "1.0-1".into(),
        TroveType::Package,
        VersionScheme::Debian,
    );
    trove.architecture = Some("amd64".into());
    trove.debian_multi_arch = Some(conary_core::repository::dependency_model::DebianMultiArch::No);
    let id = trove.insert(&conn).unwrap();
    let trove = Trove::find_by_id(&conn, id).unwrap().unwrap();
    crate::commands::test_helpers::insert_test_regular_file_with_parents(
        &conn,
        std::path::Path::new(&db_path),
        "/etc/purge-fixture.conf",
        b"configuration",
        0o644,
        id,
        None,
    );
    let mut config = ConfigFile::new(
        "/etc/purge-fixture.conf".into(),
        id,
        conary_core::hash::sha256(b"configuration"),
    );
    config.source = ConfigSource::Deb;
    config.original_md5 = crate::commands::install::config_files::debian_original_md5(
        ConfigSource::Deb,
        true,
        b"configuration",
    );
    config.insert(&conn).unwrap();
    let bundle = NativeLifecycleBundle {
        schema: NATIVE_LIFECYCLE_SCHEMA_V1.into(),
        schema_revision: NATIVE_LIFECYCLE_SCHEMA_REVISION,
        source_format: SourceFormat::Deb,
        source_family: "debian".into(),
        source_profile: Some("ubuntu-26.04".into()),
        source_release: Some("26.04".into()),
        source_arch: Some("amd64".into()),
        source_package: trove.name.clone(),
        source_version: trove.version.clone(),
        source_checksum: None,
        version_scheme: LifecycleVersionScheme::Deb,
        conversion_tool: "test".into(),
        conversion_tool_version: "1".into(),
        conversion_policy: "typed-removal-test".into(),
        evidence_digest: None,
        scriptlet_fidelity: ScriptletFidelity::NativeLifecycle,
        entries: Vec::new(),
    };
    InstalledNativeLifecycleBundle::new(id, None, &bundle)
        .unwrap()
        .insert_or_replace(&conn)
        .unwrap();
    let result = execute_installed_trove_remove_graph(
        &conn,
        &trove,
        &db_path,
        &trove.name,
        RemoveLifecycleOptions::new(crate::commands::SandboxMode::Always)
            .with_purge_config_files(true),
        &RemoveProgress::new(&trove.name),
    )
    .unwrap();
    assert!(Trove::find_by_id(&conn, id).unwrap().is_none());
    assert!(
        ConfigFile::find_by_path(&conn, "/etc/purge-fixture.conf")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        result.stats.files_removed, 1,
        "the separate native purge stage must contribute its actual removals"
    );
}
