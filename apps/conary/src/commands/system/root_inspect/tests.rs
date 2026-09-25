// apps/conary/src/commands/system/root_inspect/tests.rs

#![cfg(test)]

use super::*;
use crate::commands::generation::selected_root::{
    SelectedRootBaselineError, clear_before_current_selection_hook, create_selected_root_stand_in,
    persist_captured_publication_snapshot, persist_publication_snapshot,
    read_selected_root_baseline, read_selected_root_baseline_with_source,
    set_before_current_selection_hook, set_between_selection_and_collection_hook,
};
use crate::commands::test_helpers::create_active_test_generation;
use conary_core::db::models::{
    FileEntry, GenerationPublication, GenerationPublicationPhase, GenerationPublicationStatus,
    InstallSource, Trove, TroveType,
};
use conary_core::generation::root_manifest::{
    GENERATION_ROOT_MANIFEST_VERSION, GenerationRootEntry, GenerationRootManifest,
    MutableStateManifest, SELECTED_ROOT_MANIFEST_DELTA_VERSION, SelectedRootManifestDelta,
    SelectedRootSnapshot,
};
use conary_core::payload::PayloadNode;
use conary_core::repository::versioning::VersionScheme;

fn node(kind: PayloadNodeKind, permissions: u32) -> ResolvedPayloadNode {
    let mut source = PayloadNode::regular(0o644);
    source.kind = kind;
    source.mode = match &source.kind {
        PayloadNodeKind::Directory => libc::S_IFDIR | (permissions & 0o7777),
        PayloadNodeKind::Symlink { .. } => libc::S_IFLNK | (permissions & 0o7777),
        _ => libc::S_IFREG | (permissions & 0o7777),
    };
    source.user = PayloadIdentity::Numeric { id: 0 };
    source.group = PayloadIdentity::Numeric { id: 0 };
    ResolvedPayloadNode::from_numeric_source(source).unwrap()
}

fn directory(path: &str) -> GenerationRootEntry {
    GenerationRootEntry {
        path: path.to_string(),
        node: node(PayloadNodeKind::Directory, 0o755),
        content: None,
    }
}

fn regular(path: &str, permissions: u32, bytes: &[u8]) -> GenerationRootEntry {
    GenerationRootEntry {
        path: path.to_string(),
        node: node(
            PayloadNodeKind::Regular {
                hardlink_identity: None,
            },
            permissions,
        ),
        content: Some(PayloadContentAuthority {
            sha256: conary_core::hash::sha256(bytes),
            size: bytes.len() as u64,
        }),
    }
}

fn symlink(path: &str, target: &str) -> GenerationRootEntry {
    GenerationRootEntry {
        path: path.to_string(),
        node: node(
            PayloadNodeKind::Symlink {
                target: target.to_string(),
            },
            0o777,
        ),
        content: None,
    }
}

fn captured_root(
    immutable: Vec<GenerationRootEntry>,
    state: Vec<GenerationRootEntry>,
) -> CapturedSelectedRoot {
    CapturedSelectedRoot {
        generation: GenerationRootManifest {
            version: GENERATION_ROOT_MANIFEST_VERSION,
            root: node(PayloadNodeKind::Directory, 0o755),
            entries: immutable,
        },
        state: MutableStateManifest {
            version: GENERATION_ROOT_MANIFEST_VERSION,
            entries: state,
        },
    }
}

/// A committed snapshot with a regular file, a symlink, a mutable-state file,
/// and a path that a later delta removes.
struct Fixture {
    _temp: tempfile::TempDir,
    conn: rusqlite::Connection,
    db_path: std::path::PathBuf,
    snapshot_id: i64,
    changeset_id: i64,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("conary.db");
        conary_core::db::init(&db_path).unwrap();
        let conn = conary_core::db::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO changesets (description, status) VALUES ('inspect fixture', 'applied')",
            [],
        )
        .unwrap();
        let changeset_id = conn.last_insert_rowid();

        let captured = captured_root(
            vec![
                directory("/opt"),
                directory("/opt/fixture"),
                regular("/opt/fixture/hello", 0o644, b"hello world\n"),
                regular("/opt/fixture/removed", 0o600, b"gone\n"),
                symlink("/opt/fixture/sh", "/bin/busybox"),
            ],
            vec![
                directory("/var"),
                directory("/var/lib"),
                directory("/var/lib/fixture"),
                regular("/var/lib/fixture/state", 0o640, b"state\n"),
            ],
        );
        let debt = GenerationPublication::create_pending(
            &conn,
            Some(changeset_id),
            None,
            db_path.to_str().unwrap(),
            &temp.path().display().to_string(),
            "inspect fixture",
            &Default::default(),
        )
        .unwrap();
        let snapshot = persist_captured_publication_snapshot(&conn, &debt, &captured).unwrap();
        Self {
            _temp: temp,
            conn,
            db_path,
            snapshot_id: snapshot.id(),
            changeset_id,
        }
    }

    fn runtime_root(&self) -> ConaryRuntimeRoot {
        ConaryRuntimeRoot::from_db_path(&self.db_path)
    }

    fn inspect(&self, path: &str) -> RootInspectData {
        root_inspect_data(&self.conn, &self.runtime_root(), path).unwrap()
    }
}

/// Serialize exactly what `--json` prints and return its `data` object.
fn json_data(data: &RootInspectData) -> serde_json::Value {
    let result = inspect_result(data).unwrap();
    let value = serde_json::to_value(&result).unwrap();
    assert_eq!(value["operation"], "system.root.inspect");
    assert_eq!(value["status"], "ok");
    assert_eq!(value["risk"], "read_only");
    value["data"].clone()
}

#[test]
fn root_inspect_reports_regular_file_digest_and_symlink_target() {
    let fixture = Fixture::new();

    let file = fixture.inspect("/opt/fixture/hello");
    assert_eq!(file.source, RootInspectSource::PendingSnapshot);
    assert_eq!(file.snapshot_id, Some(fixture.snapshot_id));
    assert_eq!(file.changeset_id, Some(fixture.changeset_id));
    assert!(file.present);
    assert_eq!(file.manifest, Some(RootManifestKind::Root));
    assert_eq!(file.kind, Some(RootNodeKind::Regular));
    assert_eq!(file.metadata, RootInspectMetadata::Recorded);
    assert_eq!(file.mode, Some(0o644));
    assert_eq!(file.uid, Some(0));
    assert_eq!(file.gid, Some(0));
    let json = json_data(&file);
    assert_eq!(json["present"], true);
    assert_eq!(json["kind"], "regular");
    assert_eq!(json["metadata"], "recorded");
    assert_eq!(json["mode"], 0o644);
    assert_eq!(json["sha256"], conary_core::hash::sha256(b"hello world\n"));

    let link = fixture.inspect("/opt/fixture/sh");
    assert!(link.present);
    assert_eq!(link.kind, Some(RootNodeKind::Symlink));
    assert_eq!(link.symlink_target.as_deref(), Some("/bin/busybox"));
    let json = json_data(&link);
    assert_eq!(json["kind"], "symlink");
    assert_eq!(json["symlink_target"], "/bin/busybox");
}

#[test]
fn root_inspect_honors_lineage_tombstones_for_removed_paths() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("conary.db");
    conary_core::db::init(&db_path).unwrap();
    let conn = conary_core::db::open(&db_path).unwrap();
    let base = SelectedRootSnapshot::capture(
        &conn,
        &captured_root(
            vec![
                directory("/opt"),
                directory("/opt/fixture"),
                regular("/opt/fixture/kept", 0o644, b"kept\n"),
                regular("/opt/fixture/removed", 0o600, b"gone\n"),
            ],
            Vec::new(),
        ),
    )
    .unwrap();
    let base_debt = GenerationPublication::create_pending(
        &conn,
        None,
        None,
        db_path.to_str().unwrap(),
        &temp.path().display().to_string(),
        "base",
        &Default::default(),
    )
    .unwrap();
    persist_publication_snapshot(&conn, &base_debt, base).unwrap();

    let delta = SelectedRootManifestDelta {
        version: SELECTED_ROOT_MANIFEST_DELTA_VERSION,
        root: None,
        removals: vec!["/opt/fixture/removed".to_string()],
        opaque_directories: Vec::new(),
        upserts: Vec::new(),
    };
    let child = base.apply_delta(&conn, &delta).unwrap();
    let child_debt = GenerationPublication::create_pending(
        &conn,
        None,
        None,
        db_path.to_str().unwrap(),
        &temp.path().display().to_string(),
        "child",
        &Default::default(),
    )
    .unwrap();
    persist_publication_snapshot(&conn, &child_debt, child).unwrap();

    let runtime_root = ConaryRuntimeRoot::from_db_path(&db_path);
    let removed = root_inspect_data(&conn, &runtime_root, "/opt/fixture/removed").unwrap();
    assert_eq!(removed.source, RootInspectSource::PendingSnapshot);
    assert_eq!(removed.snapshot_id, Some(child.id()));
    assert!(!removed.present);
    assert_eq!(removed.manifest, None);
    assert_eq!(removed.kind, None);
    let json = json_data(&removed);
    assert_eq!(json["present"], false);
    assert_eq!(json["snapshot_id"], child.id());

    let kept = root_inspect_data(&conn, &runtime_root, "/opt/fixture/kept").unwrap();
    assert!(kept.present);
    assert_eq!(kept.snapshot_id, Some(child.id()));
}

#[test]
fn root_inspect_reports_mutable_state_manifest() {
    let fixture = Fixture::new();
    let state = fixture.inspect("/var/lib/fixture/state");
    assert!(state.present);
    assert_eq!(state.manifest, Some(RootManifestKind::MutableState));
    assert_eq!(state.kind, Some(RootNodeKind::Regular));
    assert_eq!(state.mode, Some(0o640));
    let json = json_data(&state);
    assert_eq!(json["manifest"], "mutable_state");
    assert_eq!(json["sha256"], conary_core::hash::sha256(b"state\n"));
}

#[test]
fn root_inspect_reports_no_committed_root_for_empty_database() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("conary.db");
    conary_core::db::init(&db_path).unwrap();
    let conn = conary_core::db::open(&db_path).unwrap();
    let runtime_root = ConaryRuntimeRoot::from_db_path(&db_path);

    let data = root_inspect_data(&conn, &runtime_root, "/opt/fixture/hello").unwrap();
    assert_eq!(data.source, RootInspectSource::NoCommittedRoot);
    assert!(!data.present);
    assert_eq!(data.snapshot_id, None);
    assert_eq!(data.changeset_id, None);
    assert_eq!(data.manifest, None);
    assert_eq!(data.kind, None);
    let json = json_data(&data);
    assert_eq!(json["source"], "no_committed_root");
    assert_eq!(json["present"], false);
}

#[test]
fn root_inspect_reports_database_projection_before_first_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("conary.db");
    conary_core::db::init(&db_path).unwrap();
    let conn = conary_core::db::open(&db_path).unwrap();
    let mut trove = Trove::new(
        "projection-fixture".to_string(),
        "1.0.0".to_string(),
        TroveType::Package,
        VersionScheme::Conary,
    );
    trove.install_source = InstallSource::Repository;
    let trove_id = trove.insert(&conn).unwrap();
    for path in ["/opt", "/opt/fixture"] {
        FileEntry::new(
            path.to_string(),
            node(PayloadNodeKind::Directory, 0o755),
            None,
            trove_id,
        )
        .insert(&conn)
        .unwrap();
    }
    FileEntry::new(
        "/opt/fixture/projected".to_string(),
        node(
            PayloadNodeKind::Regular {
                hardlink_identity: None,
            },
            0o600,
        ),
        Some(PayloadContentAuthority {
            sha256: conary_core::hash::sha256(b"projected\n"),
            size: b"projected\n".len() as u64,
        }),
        trove_id,
    )
    .insert(&conn)
    .unwrap();

    let runtime_root = ConaryRuntimeRoot::from_db_path(&db_path);
    let data = root_inspect_data(&conn, &runtime_root, "/opt/fixture/projected").unwrap();
    assert_eq!(data.source, RootInspectSource::DatabaseProjection);
    assert!(data.present);
    assert_eq!(data.manifest, Some(RootManifestKind::Root));
    assert_eq!(data.kind, Some(RootNodeKind::Regular));
    assert_eq!(data.metadata, RootInspectMetadata::Recorded);
    assert_eq!(data.mode, Some(0o600));
    let json = json_data(&data);
    assert_eq!(json["source"], "database_projection");
    assert_eq!(json["metadata"], "recorded");
    assert_eq!(json["mode"], 0o600);
    assert_eq!(json["sha256"], conary_core::hash::sha256(b"projected\n"));

    // The projection synthesizes `/` from the empty stand-in destination. The
    // node is present as a directory, but its ambient mode and ownership are
    // withheld and the record is marked synthesized so no caller mistakes the
    // inspecting process for the committed root.
    let root = root_inspect_data(&conn, &runtime_root, "/").unwrap();
    assert_eq!(root.source, RootInspectSource::DatabaseProjection);
    assert!(root.present);
    assert_eq!(root.manifest, Some(RootManifestKind::Root));
    assert_eq!(root.kind, Some(RootNodeKind::Directory));
    assert_eq!(root.metadata, RootInspectMetadata::Synthesized);
    assert_eq!(root.mode, None);
    assert_eq!(root.uid, None);
    assert_eq!(root.gid, None);
    assert_eq!(root.user, None);
    assert_eq!(root.group, None);
    let json = json_data(&root);
    assert_eq!(json["metadata"], "synthesized");
    assert!(json["mode"].is_null());
}

#[test]
fn database_projection_matches_the_main_selected_root_baseline() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("conary.db");
    conary_core::db::init(&db_path).unwrap();
    let conn = conary_core::db::open(&db_path).unwrap();
    let mut trove = Trove::new(
        "parity-fixture".to_string(),
        "1.0.0".to_string(),
        TroveType::Package,
        VersionScheme::Conary,
    );
    trove.install_source = InstallSource::Repository;
    let trove_id = trove.insert(&conn).unwrap();
    for path in ["/opt", "/opt/fixture"] {
        FileEntry::new(
            path.to_string(),
            node(PayloadNodeKind::Directory, 0o755),
            None,
            trove_id,
        )
        .insert(&conn)
        .unwrap();
    }
    let digest = conary_core::hash::sha256(b"parity\n");
    FileEntry::new(
        "/opt/fixture/parity".to_string(),
        node(
            PayloadNodeKind::Regular {
                hardlink_identity: None,
            },
            0o640,
        ),
        Some(PayloadContentAuthority {
            sha256: digest.clone(),
            size: b"parity\n".len() as u64,
        }),
        trove_id,
    )
    .insert(&conn)
    .unwrap();

    let runtime_root = ConaryRuntimeRoot::from_db_path(&db_path);
    let data = root_inspect_data(&conn, &runtime_root, "/opt/fixture/parity").unwrap();
    assert_eq!(data.source, RootInspectSource::DatabaseProjection);
    assert!(data.present);

    let empty_root_parent = tempfile::TempDir::new().unwrap();
    let empty_root = create_selected_root_stand_in(empty_root_parent.path()).unwrap();
    let captured = read_selected_root_baseline(&conn, &runtime_root, &empty_root).unwrap();
    let entry = captured
        .generation
        .entries
        .iter()
        .find(|entry| entry.path == "/opt/fixture/parity")
        .expect("the baseline projection must own the installed regular file");

    assert_eq!(data.manifest, Some(RootManifestKind::Root));
    assert_eq!(data.kind, Some(node_kind(&entry.node.source.kind)));
    assert_eq!(data.mode, Some(entry.node.source.mode & 0o7777));
    assert_eq!(
        data.sha256,
        entry.content.as_ref().map(|c| c.sha256.clone())
    );
    assert_eq!(data.sha256.as_deref(), Some(digest.as_str()));
}

/// The database-projection source and its capture must come from one database
/// snapshot. This commits a new generation-input trove on a second connection
/// between the selecting query and the collecting reads; snapshot isolation
/// keeps the capture at the selection's view.
#[test]
fn database_projection_selects_and_collects_from_one_read_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("conary.db");
    conary_core::db::init(&db_path).unwrap();
    let conn = conary_core::db::open(&db_path).unwrap();
    let mut trove = Trove::new(
        "snapshot-fixture".to_string(),
        "1.0.0".to_string(),
        TroveType::Package,
        VersionScheme::Conary,
    );
    trove.install_source = InstallSource::Repository;
    let trove_id = trove.insert(&conn).unwrap();
    for path in ["/opt", "/opt/fixture"] {
        FileEntry::new(
            path.to_string(),
            node(PayloadNodeKind::Directory, 0o755),
            None,
            trove_id,
        )
        .insert(&conn)
        .unwrap();
    }
    FileEntry::new(
        "/opt/fixture/kept".to_string(),
        node(
            PayloadNodeKind::Regular {
                hardlink_identity: None,
            },
            0o644,
        ),
        Some(PayloadContentAuthority {
            sha256: conary_core::hash::sha256(b"kept\n"),
            size: b"kept\n".len() as u64,
        }),
        trove_id,
    )
    .insert(&conn)
    .unwrap();

    let writer = conary_core::db::open(&db_path).unwrap();
    let intruder_hash = conary_core::hash::sha256(b"intruder\n");
    set_between_selection_and_collection_hook(move || {
        let mut intruder = Trove::new(
            "intruder".to_string(),
            "1.0.0".to_string(),
            TroveType::Package,
            VersionScheme::Conary,
        );
        intruder.install_source = InstallSource::Repository;
        let intruder_id = intruder.insert(&writer).unwrap();
        FileEntry::new(
            "/opt/fixture/intruder".to_string(),
            node(
                PayloadNodeKind::Regular {
                    hardlink_identity: None,
                },
                0o600,
            ),
            Some(PayloadContentAuthority {
                sha256: intruder_hash,
                size: b"intruder\n".len() as u64,
            }),
            intruder_id,
        )
        .insert(&writer)
        .unwrap();
    });

    let runtime_root = ConaryRuntimeRoot::from_db_path(&db_path);
    let empty_root_parent = tempfile::TempDir::new().unwrap();
    let empty_root = create_selected_root_stand_in(empty_root_parent.path()).unwrap();
    let (source, captured) =
        read_selected_root_baseline_with_source(&conn, &runtime_root, &empty_root).unwrap();

    assert!(
        matches!(source, SelectedRootSource::DatabaseProjection { .. }),
        "expected a database projection, got {source:?}"
    );
    assert!(
        captured
            .generation
            .entries
            .iter()
            .any(|entry| entry.path == "/opt/fixture/kept"),
        "the selecting snapshot must remain in the capture"
    );
    assert!(
        !captured
            .generation
            .entries
            .iter()
            .any(|entry| entry.path == "/opt/fixture/intruder"),
        "a commit between selection and collection must stay outside the capture"
    );
}

/// A caller that already holds a savepoint must reuse its snapshot rather than
/// attempt a nested BEGIN, which SQLite rejects.
#[test]
fn read_baseline_reuses_a_caller_owned_savepoint() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("conary.db");
    conary_core::db::init(&db_path).unwrap();
    let mut conn = conary_core::db::open(&db_path).unwrap();
    let runtime_root = ConaryRuntimeRoot::from_db_path(&db_path);
    let empty_root_parent = tempfile::TempDir::new().unwrap();
    let empty_root = create_selected_root_stand_in(empty_root_parent.path()).unwrap();

    let savepoint = conn.savepoint().unwrap();
    let (source, _captured) =
        read_selected_root_baseline_with_source(&savepoint, &runtime_root, &empty_root).unwrap();
    assert_eq!(source, SelectedRootSource::NoCommittedRoot);
    savepoint.commit().unwrap();
}

#[test]
fn root_inspect_reports_published_generation_when_no_snapshot_exists() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("conary.db");
    conary_core::db::init(&db_path).unwrap();
    create_active_test_generation(&db_path, 1);
    let conn = conary_core::db::open(&db_path).unwrap();
    let runtime_root = ConaryRuntimeRoot::from_db_path(&db_path);

    let data = root_inspect_data(&conn, &runtime_root, "/sbin/init").unwrap();
    assert_eq!(data.source, RootInspectSource::CurrentGeneration);
    assert!(data.present);
    assert_eq!(data.kind, Some(RootNodeKind::Regular));
    assert_eq!(data.mode, Some(0o755));
    let json = json_data(&data);
    assert_eq!(json["source"], "current_generation");
    assert_eq!(
        json["sha256"],
        conary_core::hash::sha256(b"test init binary")
    );

    // A generation artifact records its root node, so `/` keeps its committed
    // mode and ownership instead of the projection's synthesized stand-in.
    let root = root_inspect_data(&conn, &runtime_root, "/").unwrap();
    assert_eq!(root.source, RootInspectSource::CurrentGeneration);
    assert!(root.present);
    assert_eq!(root.kind, Some(RootNodeKind::Directory));
    assert_eq!(root.metadata, RootInspectMetadata::Recorded);
    assert_eq!(root.mode, Some(0o755));
}

/// Publish generation `generation` exactly as a concurrent publication would:
/// build the artifact/state/`/current` link, then record a terminal generation
/// publication bound to a selected-root snapshot. Returns the snapshot id.
fn publish_generation_with_snapshot(db_path: &std::path::Path, generation: i64) -> i64 {
    create_active_test_generation(db_path, generation);
    let conn = conary_core::db::open(db_path).unwrap();
    let runtime_root = ConaryRuntimeRoot::from_db_path(db_path.to_path_buf());
    let debt = GenerationPublication::create_pending(
        &conn,
        None,
        None,
        db_path.to_str().unwrap(),
        &runtime_root.root().display().to_string(),
        "concurrent publication fixture",
        &Default::default(),
    )
    .unwrap();
    let captured = captured_root(
        vec![
            directory("/sbin"),
            regular(
                &format!("/sbin/generation-{generation}"),
                0o644,
                format!("generation {generation}\n").as_bytes(),
            ),
        ],
        Vec::new(),
    );
    let snapshot = persist_captured_publication_snapshot(&conn, &debt, &captured).unwrap();
    debt.set_phase(
        &conn,
        GenerationPublicationPhase::DatabaseBackedUp,
        GenerationPublicationStatus::Running,
        Some(generation),
        Some(generation),
    )
    .unwrap();
    debt.mark_complete_through(&conn, None, generation, generation)
        .unwrap();
    snapshot.id()
}

/// `/current` is not covered by the SQLite snapshot. A publication that swaps
/// the link after the pinned transaction began must not pair the new artifact
/// with the old snapshot's authority; the selection re-pins and reports the new
/// generation's own selected-root snapshot.
#[test]
fn current_generation_selection_retries_when_current_advances_past_the_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("conary.db");
    conary_core::db::init(&db_path).unwrap();
    create_active_test_generation(&db_path, 1);
    let conn = conary_core::db::open(&db_path).unwrap();
    let runtime_root = ConaryRuntimeRoot::from_db_path(&db_path);
    let empty_root_parent = tempfile::TempDir::new().unwrap();
    let empty_root = create_selected_root_stand_in(empty_root_parent.path()).unwrap();

    let published = std::sync::Arc::new(std::sync::Mutex::new(None::<i64>));
    let hook_path = db_path.clone();
    let hook_published = published.clone();
    set_before_current_selection_hook(move || {
        let mut slot = hook_published.lock().unwrap();
        if slot.is_none() {
            *slot = Some(publish_generation_with_snapshot(&hook_path, 2));
        }
    });

    let (source, _captured) =
        read_selected_root_baseline_with_source(&conn, &runtime_root, &empty_root).unwrap();

    let expected_snapshot = published
        .lock()
        .unwrap()
        .expect("the hook must publish the advancing generation");
    assert_eq!(
        source,
        SelectedRootSource::CurrentGeneration {
            snapshot_id: Some(expected_snapshot),
            changeset_id: None,
        }
    );
    assert_eq!(
        conary_core::generation::mount::current_generation(runtime_root.root()).unwrap(),
        Some(2)
    );
    clear_before_current_selection_hook();
}

/// A hook that keeps advancing `/current` cannot be reconciled with any pinned
/// snapshot, so the read refuses with the typed retry error after the attempts
/// are exhausted instead of returning a stale pairing.
#[test]
fn current_generation_selection_refuses_after_the_attempt_limit() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("conary.db");
    conary_core::db::init(&db_path).unwrap();
    create_active_test_generation(&db_path, 1);
    let conn = conary_core::db::open(&db_path).unwrap();
    let runtime_root = ConaryRuntimeRoot::from_db_path(&db_path);
    let empty_root_parent = tempfile::TempDir::new().unwrap();
    let empty_root = create_selected_root_stand_in(empty_root_parent.path()).unwrap();

    let next_generation = std::sync::Arc::new(std::sync::Mutex::new(2_i64));
    let hook_path = db_path.clone();
    let hook_next = next_generation.clone();
    set_before_current_selection_hook(move || {
        let mut next = hook_next.lock().unwrap();
        let generation = *next;
        *next += 1;
        drop(next);
        create_active_test_generation(&hook_path, generation);
    });

    let error = read_selected_root_baseline_with_source(&conn, &runtime_root, &empty_root)
        .expect_err("a /current that keeps moving must exhaust the attempts");
    let typed = error
        .downcast_ref::<SelectedRootBaselineError>()
        .expect("the refusal must be the typed current-generation error");
    assert!(matches!(
        typed,
        SelectedRootBaselineError::CurrentGenerationChanged
    ));
    clear_before_current_selection_hook();
}

#[test]
fn root_inspect_normalizes_lookup_paths_without_symlink_resolution() {
    let fixture = Fixture::new();

    let normalized = fixture.inspect("opt/./fixture//hello");
    assert!(normalized.present);
    assert_eq!(normalized.path, "/opt/fixture/hello");

    let link = fixture.inspect("/opt/fixture/sh");
    assert_eq!(link.kind, Some(RootNodeKind::Symlink));
}
