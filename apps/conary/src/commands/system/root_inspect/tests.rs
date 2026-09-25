// apps/conary/src/commands/system/root_inspect/tests.rs

#![cfg(test)]

use super::*;
use crate::commands::generation::selected_root::{
    SelectedRootBaseline, SelectedRootBaselineError, clear_before_current_selection_hook,
    persist_captured_publication_snapshot, persist_publication_snapshot,
    read_selected_root_baseline, set_before_current_selection_hook,
    set_between_selection_and_collection_hook,
};
use crate::commands::test_helpers::create_active_test_generation;
use conary_core::db::models::{
    CreateTrySession, FileEntry, GenerationPublication, GenerationPublicationPhase,
    GenerationPublicationStatus, InstallSource, SystemState, Trove, TroveType, TrySession,
    TrySessionMode,
};
use conary_core::generation::root_manifest::{
    GENERATION_ROOT_MANIFEST_VERSION, GenerationRootEntry, GenerationRootManifest,
    MutableStateManifest, SELECTED_ROOT_MANIFEST_DELTA_VERSION, SelectedRootManifestDelta,
    SelectedRootSnapshot,
};
use conary_core::payload::PayloadNode;
use conary_core::repository::versioning::VersionScheme;

/// Serializes tests that mutate process environment variables.
///
/// `TMPDIR` is read by every `tempfile` constructor in the process, so a test
/// that points it at an unusable path must not race the rest of the suite. The
/// exact-child process below runs this one test, and the guard restores the
/// previous value when it ends.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock_env() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Set one process environment variable and restore its previous value on drop.
struct EnvVarGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let previous = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        unsafe {
            if let Some(previous) = &self.previous {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}

const TMPDIR_CHILD_SCENARIO: &str = "CONARY_ROOT_INSPECT_TMPDIR_SCENARIO";
const TMPDIR_CHILD_DB: &str = "CONARY_ROOT_INSPECT_TMPDIR_DB";
const TMPDIR_CHILD_BAD_DIR: &str = "CONARY_ROOT_INSPECT_TMPDIR_BAD_DIR";
const TMPDIR_CHILD_MARKER: &str = "CONARY_ROOT_INSPECT_TMPDIR_MARKER";
const TMPDIR_CHILD_TEST: &str =
    "commands::system::root_inspect::tests::root_inspect_with_unavailable_tmpdir_child";

/// Run the exact child test with an unusable `TMPDIR`.
///
/// The child sets `TMPDIR` itself, so the bad value never reaches a concurrently
/// running test. The marker file proves the exact child test ran and passed
/// rather than being filtered out to an empty, trivially successful run.
fn run_tmpdir_child(scenario: &str, db_path: &std::path::Path, bad_tmpdir: &std::path::Path) {
    let temp = tempfile::tempdir().unwrap();
    let marker = temp.path().join("child-ran");
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    crate::test_hooks::clear_inherited_hooks(&mut command);
    command
        .args(["--exact", TMPDIR_CHILD_TEST, "--nocapture"])
        .env(TMPDIR_CHILD_SCENARIO, scenario)
        .env(TMPDIR_CHILD_DB, db_path)
        .env(TMPDIR_CHILD_BAD_DIR, bad_tmpdir)
        .env(TMPDIR_CHILD_MARKER, &marker);
    let status = command
        .status()
        .expect("spawn the unavailable-TMPDIR inspection child");
    assert!(
        status.success(),
        "the {scenario} unavailable-TMPDIR child test must pass"
    );
    assert_eq!(
        std::fs::read_to_string(&marker).expect("the exact child test must run"),
        scenario,
    );
}

/// Prove the child ran the same inspection under an unusable `TMPDIR`.
#[test]
fn root_inspect_with_unavailable_tmpdir_child() {
    let Ok(scenario) = std::env::var(TMPDIR_CHILD_SCENARIO) else {
        return;
    };
    let _env_lock = lock_env();
    let db_path = std::path::PathBuf::from(
        std::env::var_os(TMPDIR_CHILD_DB).expect("the child database path"),
    );
    let bad_tmpdir =
        std::env::var_os(TMPDIR_CHILD_BAD_DIR).expect("the child unusable TMPDIR path");
    let marker = std::env::var_os(TMPDIR_CHILD_MARKER).expect("the child marker path");
    let _tmpdir = EnvVarGuard::set("TMPDIR", &bad_tmpdir);

    let conn = conary_core::db::open(&db_path).unwrap();
    let runtime_root = ConaryRuntimeRoot::from_db_path(&db_path);

    match scenario.as_str() {
        "current_generation" => {
            let data = root_inspect_data(&conn, &runtime_root, "/sbin/init").unwrap();
            assert_eq!(data.source, RootInspectSource::CurrentGeneration);
            assert!(data.present);
            assert_eq!(data.kind, Some(RootNodeKind::Regular));
        }
        "database_projection" => {
            let error = root_inspect_data(&conn, &runtime_root, "/opt/fixture/projected")
                .expect_err("a database projection must need the empty stand-in");
            let io_error = error
                .chain()
                .find_map(|cause| cause.downcast_ref::<std::io::Error>())
                .expect("the temp-directory failure must be the typed io cause");
            assert_eq!(io_error.kind(), std::io::ErrorKind::NotFound);
        }
        "no_committed_root" => {
            let data = root_inspect_data(&conn, &runtime_root, "/opt/fixture/hello").unwrap();
            assert_eq!(data.source, RootInspectSource::NoCommittedRoot);
            assert!(!data.present);
            let json = json_data(&data);
            assert_eq!(json["source"], "no_committed_root");
            assert_eq!(json["present"], false);
        }
        other => panic!("unknown unavailable-TMPDIR scenario {other}"),
    }

    std::fs::write(marker, &scenario).expect("record the successful child scenario");
}

fn node(kind: PayloadNodeKind, permissions: u32) -> ResolvedPayloadNode {
    let mut source = PayloadNode::regular(0o644);
    source.kind = kind;
    source.mode = match &source.kind {
        PayloadNodeKind::Directory => libc::S_IFDIR | (permissions & 0o7777),
        PayloadNodeKind::Symlink { .. } => libc::S_IFLNK | (permissions & 0o7777),
        PayloadNodeKind::BlockDevice { .. } => libc::S_IFBLK | (permissions & 0o7777),
        PayloadNodeKind::CharacterDevice { .. } => libc::S_IFCHR | (permissions & 0o7777),
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

fn device(path: &str, kind: PayloadNodeKind) -> GenerationRootEntry {
    GenerationRootEntry {
        path: path.to_string(),
        node: node(kind, 0o600),
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

/// A committed snapshot with regular files, a symlink, device nodes, a
/// mutable-state file, and a path that a later delta removes.
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
                device(
                    "/opt/fixture/null",
                    PayloadNodeKind::CharacterDevice { major: 1, minor: 3 },
                ),
                regular("/opt/fixture/removed", 0o600, b"gone\n"),
                device(
                    "/opt/fixture/sda",
                    PayloadNodeKind::BlockDevice { major: 8, minor: 0 },
                ),
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
fn root_inspect_reports_device_numbers_for_device_nodes() {
    let fixture = Fixture::new();

    let character = fixture.inspect("/opt/fixture/null");
    assert!(character.present);
    assert_eq!(character.kind, Some(RootNodeKind::CharacterDevice));
    assert_eq!(character.device_major, Some(1));
    assert_eq!(character.device_minor, Some(3));
    let json = json_data(&character);
    assert_eq!(json["kind"], "character_device");
    assert_eq!(json["device_major"], 1);
    assert_eq!(json["device_minor"], 3);

    let block = fixture.inspect("/opt/fixture/sda");
    assert!(block.present);
    assert_eq!(block.kind, Some(RootNodeKind::BlockDevice));
    assert_eq!(block.device_major, Some(8));
    assert_eq!(block.device_minor, Some(0));
    let json = json_data(&block);
    assert_eq!(json["kind"], "block_device");
    assert_eq!(json["device_major"], 8);
    assert_eq!(json["device_minor"], 0);

    // Control: a regular file has no device identity, so both fields are null.
    let file = fixture.inspect("/opt/fixture/hello");
    assert_eq!(file.kind, Some(RootNodeKind::Regular));
    assert_eq!(file.device_major, None);
    assert_eq!(file.device_minor, None);
    let json = json_data(&file);
    assert_eq!(json["kind"], "regular");
    assert!(json["device_major"].is_null());
    assert!(json["device_minor"].is_null());
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

/// A current-generation artifact is read straight from its typed manifest. It
/// must inspect successfully even when `TMPDIR` names a directory that does not
/// exist, because no branch of that read creates the projection stand-in.
#[test]
fn current_generation_inspection_succeeds_when_tmpdir_is_unavailable() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("conary.db");
    conary_core::db::init(&db_path).unwrap();
    create_active_test_generation(&db_path, 1);
    let bad_tmpdir = temp.path().join("no-such-tmpdir");

    run_tmpdir_child("current_generation", &db_path, &bad_tmpdir);
}

/// A database projection still needs the empty materialization stand-in. With
/// the same unusable `TMPDIR` it must fail while creating that private
/// directory, proving the stand-in is now made only on this branch.
#[test]
fn database_projection_inspection_fails_when_tmpdir_is_unavailable() {
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
    trove.insert(&conn).unwrap();
    drop(conn);
    let bad_tmpdir = temp.path().join("no-such-tmpdir");

    run_tmpdir_child("database_projection", &db_path, &bad_tmpdir);
}

/// An initialized database with no troves has no committed root, so its
/// inspection must not need `TMPDIR`: the absent-database branch returns the
/// typed `NoCommittedRoot` result before any stand-in allocation. The same
/// unusable `TMPDIR` must therefore still succeed, which the child proves.
#[test]
fn empty_database_inspection_succeeds_when_tmpdir_is_unavailable() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("conary.db");
    conary_core::db::init(&db_path).unwrap();
    let bad_tmpdir = temp.path().join("no-such-tmpdir");

    run_tmpdir_child("no_committed_root", &db_path, &bad_tmpdir);
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

    let SelectedRootBaseline::Captured { captured, .. } =
        read_selected_root_baseline(&conn, &runtime_root).unwrap()
    else {
        panic!("a present database projection must supply a captured baseline");
    };
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
    let SelectedRootBaseline::Captured { source, captured } =
        read_selected_root_baseline(&conn, &runtime_root).unwrap()
    else {
        panic!("a present database projection must supply a captured baseline");
    };

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

    let savepoint = conn.savepoint().unwrap();
    let baseline = read_selected_root_baseline(&savepoint, &runtime_root).unwrap();
    assert!(matches!(baseline, SelectedRootBaseline::NoCommittedRoot));
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
    assert!(!data.recovered_without_state);
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

/// Build generation `generation` with a valid artifact and `/current` link but
/// no `SystemState` and no `GenerationPublication` row: the exact shape boot
/// recovery leaves (`recovery.rs` `mark_generation_state_active_if_present`
/// accepts a missing state snapshot and writes no publication row).
fn create_state_less_test_generation(db_path: &std::path::Path, generation: i64) {
    create_active_test_generation(db_path, generation);
    let conn = conary_core::db::open(db_path).unwrap();
    let state = SystemState::find_by_number(&conn, generation)
        .unwrap()
        .expect("the test helper starts with a state row to remove");
    SystemState::delete(&conn, state.id.unwrap()).unwrap();
    let removed_state = SystemState::find_by_number(&conn, generation).unwrap();
    assert!(removed_state.is_none());
    assert!(
        GenerationPublication::completed_for_generation(&conn, generation)
            .unwrap()
            .is_none(),
        "a state-less recovery generation must have no terminal publication row"
    );
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

    let published = std::sync::Arc::new(std::sync::Mutex::new(None::<i64>));
    let hook_path = db_path.clone();
    let hook_published = published.clone();
    set_before_current_selection_hook(move || {
        let mut slot = hook_published.lock().unwrap();
        if slot.is_none() {
            *slot = Some(publish_generation_with_snapshot(&hook_path, 2));
        }
    });

    let SelectedRootBaseline::Captured { source, .. } =
        read_selected_root_baseline(&conn, &runtime_root).unwrap()
    else {
        panic!("an advancing current generation must supply a captured baseline");
    };

    let expected_snapshot = published
        .lock()
        .unwrap()
        .expect("the hook must publish the advancing generation");
    assert_eq!(
        source,
        SelectedRootSource::CurrentGeneration {
            snapshot_id: Some(expected_snapshot),
            changeset_id: None,
            recovered_without_state: false,
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

    let error = read_selected_root_baseline(&conn, &runtime_root)
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

/// Boot recovery can point `/current` at a valid generation artifact that has
/// neither a state snapshot nor a terminal publication row. When the link is
/// stable across the read, inspection must accept the artifact and mark the
/// database IDs unknown rather than refusing forever.
#[test]
fn root_inspect_accepts_a_stable_current_generation_without_state_rows() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("conary.db");
    conary_core::db::init(&db_path).unwrap();
    create_state_less_test_generation(&db_path, 1);
    let conn = conary_core::db::open(&db_path).unwrap();
    let runtime_root = ConaryRuntimeRoot::from_db_path(&db_path);

    let data = root_inspect_data(&conn, &runtime_root, "/sbin/init").unwrap();
    assert_eq!(data.source, RootInspectSource::CurrentGeneration);
    assert!(
        data.recovered_without_state,
        "a state-less recovery generation must be marked recovered"
    );
    assert_eq!(data.snapshot_id, None);
    assert_eq!(data.changeset_id, None);
    assert!(data.present);
    assert_eq!(data.kind, Some(RootNodeKind::Regular));
    assert_eq!(data.mode, Some(0o755));
    let json = json_data(&data);
    assert_eq!(json["source"], "current_generation");
    assert_eq!(json["recovered_without_state"], true);
    assert!(json["snapshot_id"].is_null());
    assert!(json["changeset_id"].is_null());
    assert_eq!(
        json["sha256"],
        conary_core::hash::sha256(b"test init binary")
    );
}

/// An activated try session builds its generation from the copied try database
/// and publishes it as the live `/current`, while the live database has no
/// state row or terminal publication for it. That state-less link is an
/// uncommitted trial, not boot recovery: inspection must refuse with the typed
/// try-session error instead of reporting the trial payload as committed.
#[test]
fn root_inspect_refuses_a_state_less_current_generation_claimed_by_a_try_session() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("conary.db");
    conary_core::db::init(&db_path).unwrap();
    create_state_less_test_generation(&db_path, 1);
    let conn = conary_core::db::open(&db_path).unwrap();
    let runtime_root = ConaryRuntimeRoot::from_db_path(&db_path);

    // Positive control: with no try session the same state-less generation is
    // the accepted boot-recovery baseline.
    let recovered = root_inspect_data(&conn, &runtime_root, "/sbin/init").unwrap();
    assert_eq!(recovered.source, RootInspectSource::CurrentGeneration);
    assert!(recovered.recovered_without_state);

    // Record an activated try session that owns that exact generation.
    let session = TrySession::create_active(
        &conn,
        CreateTrySession {
            id: "try-fixture",
            package_path: "/fixture/package.ccs",
            package_signing_key: "fixture-signing-key",
            package_name: Some("try-fixture"),
            package_version: Some("1.0.0"),
            previous_generation_id: None,
            mode: TrySessionMode::Activated,
            work_dir: "/fixture/work",
        },
    )
    .unwrap();
    session.set_try_generation(&conn, 1).unwrap();

    let error = root_inspect_data(&conn, &runtime_root, "/sbin/init")
        .expect_err("a try session's state-less /current is not recovery");
    let typed = error
        .downcast_ref::<SelectedRootBaselineError>()
        .expect("the refusal must be the typed baseline error");
    assert!(
        matches!(typed, SelectedRootBaselineError::TrySessionOwnsCurrent),
        "the refusal must come from the try-session rule, got {typed}"
    );
}

/// The stable-link acceptance is bracketed by reads before and after the
/// snapshot. A state-less target that moves after the selection but before the
/// trailing read must not be accepted; the retry selects the generation the
/// link settled on and reports its recorded snapshot instead.
#[test]
fn root_inspect_rejects_a_state_less_link_that_moves_after_selection() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("conary.db");
    conary_core::db::init(&db_path).unwrap();
    create_state_less_test_generation(&db_path, 1);
    let conn = conary_core::db::open(&db_path).unwrap();
    let runtime_root = ConaryRuntimeRoot::from_db_path(&db_path);

    let published = std::sync::Arc::new(std::sync::Mutex::new(None::<i64>));
    let hook_path = db_path.clone();
    let hook_published = published.clone();
    set_between_selection_and_collection_hook(move || {
        *hook_published.lock().unwrap() = Some(publish_generation_with_snapshot(&hook_path, 2));
    });

    let data = root_inspect_data(&conn, &runtime_root, "/sbin/init").unwrap();

    let expected_snapshot = published
        .lock()
        .unwrap()
        .expect("the hook must publish the advancing generation");
    assert_eq!(
        data.source,
        RootInspectSource::CurrentGeneration,
        "the retry must select the generation the link settled on"
    );
    assert!(
        !data.recovered_without_state,
        "the moved-to generation recorded its own snapshot and must not be marked recovered"
    );
    assert_eq!(data.snapshot_id, Some(expected_snapshot));
    assert_eq!(
        conary_core::generation::mount::current_generation(runtime_root.root()).unwrap(),
        Some(2)
    );
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

/// A pending publication snapshot is authoritative without `/current`. A
/// dangling link must not stop the read before the snapshot can be selected.
#[test]
fn pending_snapshot_inspection_ignores_a_dangling_current_link() {
    let fixture = Fixture::new();
    let runtime_root = fixture.runtime_root();
    std::os::unix::fs::symlink(
        runtime_root.generations_dir().join("999"),
        runtime_root.current_link(),
    )
    .unwrap();

    let data = fixture.inspect("/opt/fixture/hello");
    assert_eq!(data.source, RootInspectSource::PendingSnapshot);
    assert_eq!(data.snapshot_id, Some(fixture.snapshot_id));
    assert!(data.present);
    assert_eq!(data.kind, Some(RootNodeKind::Regular));
}

/// The dangling link is still refused when no pending snapshot can take
/// authority ahead of it, proving the positive case selects the snapshot
/// rather than swallowing every `/current` failure.
#[test]
fn dangling_current_link_without_a_pending_snapshot_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("conary.db");
    conary_core::db::init(&db_path).unwrap();
    let conn = conary_core::db::open(&db_path).unwrap();
    let runtime_root = ConaryRuntimeRoot::from_db_path(&db_path);
    std::os::unix::fs::symlink(
        runtime_root.generations_dir().join("999"),
        runtime_root.current_link(),
    )
    .unwrap();

    let error = root_inspect_data(&conn, &runtime_root, "/opt/fixture/hello")
        .expect_err("a dangling /current with no pending snapshot must refuse");
    let typed = error
        .downcast_ref::<conary_core::Error>()
        .expect("the refusal must retain the typed current-link error");
    assert!(matches!(typed, conary_core::Error::IoError(_)));
}

#[test]
fn root_inspect_refuses_an_empty_database_file_without_initializing_it() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("empty.db");
    std::fs::write(&db_path, []).unwrap();
    assert_eq!(std::fs::metadata(&db_path).unwrap().len(), 0);

    let error = cmd_root_inspect(db_path.to_str().unwrap(), "/opt/fixture/hello", true)
        .expect_err("an empty database file must be refused, not initialized");
    let typed = error
        .downcast_ref::<conary_core::Error>()
        .expect("the refusal must retain the typed schema error");
    assert!(
        matches!(
            typed,
            conary_core::Error::SchemaRebuildRequired { observed, .. }
                if observed == "fresh database"
        ),
        "an empty file must be the typed fresh-database refusal, got {typed}"
    );

    assert_eq!(
        std::fs::metadata(&db_path).unwrap().len(),
        0,
        "inspection must not write a schema header into an empty file"
    );
    assert_eq!(
        conary_core::db::schema::inspect(&db_path).unwrap(),
        conary_core::db::schema::SchemaCompatibility::Fresh,
        "the file must remain a fresh database with no tables"
    );
}

/// Why a mode-0o444 database cannot prove read-only inspection here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadOnlyDatabaseSkip {
    /// Root bypasses the file mode, so it cannot exercise a denied write.
    EffectiveUserIsRoot,
}

impl ReadOnlyDatabaseSkip {
    /// `Ok(())` when the mode denies this process writes, else the typed skip.
    fn detect(db_path: &std::path::Path) -> std::result::Result<(), Self> {
        if nix::unistd::Uid::effective().is_root() {
            return Err(Self::EffectiveUserIsRoot);
        }
        assert!(
            std::fs::OpenOptions::new()
                .write(true)
                .open(db_path)
                .is_err(),
            "a mode-0o444 database must deny a non-root write before the proof"
        );
        Ok(())
    }
}

/// The live opener reads a mode-0o444 database. SQLite's WAL reader still needs
/// write access to the `-shm` wal-index, and to the containing directory when it
/// must create that `-shm`, so the fixture leaves the directory writable and
/// checkpoints the WAL first: the mode-0o444 database file is the only
/// read-only part, and `-shm` initialization never has to recover frames.
#[test]
fn root_inspect_reads_a_read_only_database() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("conary.db");
    conary_core::db::init(&db_path).unwrap();
    {
        let conn = conary_core::db::open(&db_path).unwrap();
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .unwrap();
        let journal_mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode, "wal", "the fixture must stay in WAL mode");
    }
    std::fs::set_permissions(&db_path, std::fs::Permissions::from_mode(0o444)).unwrap();

    if let Err(skip) = ReadOnlyDatabaseSkip::detect(&db_path) {
        // The only honored skip reason is root bypassing the file mode.
        assert_eq!(skip, ReadOnlyDatabaseSkip::EffectiveUserIsRoot);
        return;
    }

    cmd_root_inspect(db_path.to_str().unwrap(), "/opt/fixture/hello", true)
        .expect("a readable, non-writable current-schema database must inspect");
}

/// A write committed on an open connection, not yet checkpointed, lives only in
/// the `-wal`. The live opener reads that snapshot and inspection sees the
/// committed file; the offline immutable opener refuses the same state.
#[test]
fn root_inspect_reads_a_live_uncheckpointed_wal_commit() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("conary.db");
    conary_core::db::init(&db_path).unwrap();
    let conn = conary_core::db::open(&db_path).unwrap();
    let mut trove = Trove::new(
        "live-wal-fixture".to_string(),
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
    let digest = conary_core::hash::sha256(b"live wal\n");
    FileEntry::new(
        "/opt/fixture/live".to_string(),
        node(
            PayloadNodeKind::Regular {
                hardlink_identity: None,
            },
            0o644,
        ),
        Some(PayloadContentAuthority {
            sha256: digest.clone(),
            size: b"live wal\n".len() as u64,
        }),
        trove_id,
    )
    .insert(&conn)
    .unwrap();

    // The writer stays open, so the commit sits in the -wal with active frames.
    let wal_path = db_path.with_extension("db-wal");
    assert!(
        std::fs::metadata(&wal_path).unwrap().len() > 0,
        "the commit must remain in the WAL, not be checkpointed"
    );

    let reader = conary_core::db::open_live_read_only(&db_path).unwrap();
    let runtime_root = ConaryRuntimeRoot::from_db_path(&db_path);
    let data = root_inspect_data(&reader, &runtime_root, "/opt/fixture/live").unwrap();
    assert_eq!(data.source, RootInspectSource::DatabaseProjection);
    assert!(data.present, "inspection must see the committed WAL frame");
    assert_eq!(data.metadata, RootInspectMetadata::Recorded);
    assert_eq!(data.kind, Some(RootNodeKind::Regular));
    assert_eq!(data.sha256.as_deref(), Some(digest.as_str()));

    // Control: the offline immutable opener refuses the same live WAL with its
    // typed active-frame error, showing why the live opener is required.
    let refusal = conary_core::db::open_read_only(&db_path).unwrap_err();
    assert!(
        matches!(refusal, conary_core::Error::ConflictError(_)),
        "the offline opener must refuse active WAL frames, got {refusal}"
    );
}

#[test]
fn root_inspect_reads_a_normal_database() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("conary.db");
    conary_core::db::init(&db_path).unwrap();

    cmd_root_inspect(db_path.to_str().unwrap(), "/opt/fixture/hello", true)
        .expect("a normal current-schema database must inspect");
}
