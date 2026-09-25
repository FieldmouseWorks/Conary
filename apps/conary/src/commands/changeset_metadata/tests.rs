// apps/conary/src/commands/changeset_metadata/tests.rs

use super::*;
use conary_core::generation::root_manifest::{
    CapturedSelectedRoot, GENERATION_ROOT_MANIFEST_VERSION, GenerationRootManifest,
    MutableStateManifest,
};
use conary_core::payload::{
    PayloadContentAuthority, PayloadNode, PayloadNodeKind, ResolvedPayloadNode,
};

fn rollback_root(conn: &rusqlite::Connection) -> SelectedRootSnapshot {
    let mut root = PayloadNode::regular(0o755);
    root.kind = PayloadNodeKind::Directory;
    root.mode = libc::S_IFDIR | 0o755;
    let captured = CapturedSelectedRoot {
        generation: GenerationRootManifest {
            version: GENERATION_ROOT_MANIFEST_VERSION,
            root: ResolvedPayloadNode::from_numeric_source(root).unwrap(),
            entries: Vec::new(),
        },
        state: MutableStateManifest::empty(),
    };
    SelectedRootSnapshot::capture(conn, &captured).unwrap()
}

fn snapshot(name: &str) -> TroveSnapshot {
    let mut snapshot = TroveSnapshot::test_package(
        name,
        "1.0.0",
        vec![FileSnapshot {
            path: "/usr/bin/fixture".to_string(),
            node: ResolvedPayloadNode::from_numeric_source(PayloadNode::regular(0o755)).unwrap(),
            content: Some(PayloadContentAuthority {
                sha256: "0".repeat(64),
                size: 7,
            }),
            installed_at: "2026-01-01T00:00:00Z".to_string(),
            component: None,
        }],
    );
    snapshot.architecture = Some("x86_64".to_string());
    snapshot.install_source = conary_core::db::models::InstallSource::Repository;
    snapshot
}

#[test]
fn parses_versioned_envelope_snapshots_and_deferred_follow_up() {
    let warning = DeferredFollowUp {
        kind: "state_snapshot".to_string(),
        status: "failed".to_string(),
        message: "root is not self-contained".to_string(),
        retry_command: Some("conary system state create \"retry\"".to_string()),
    };
    let raw = metadata_with_envelope_sections(
        vec![snapshot("fixture")],
        true,
        Some(RollbackSystemAuthority::default()),
        Some(Vec::new()),
        vec![warning.clone()],
        Vec::new(),
    )
    .unwrap();

    let parsed = parse_rollback_snapshots(&raw).unwrap();
    let deferred = deferred_follow_up(Some(&raw)).unwrap();

    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].name, "fixture");
    assert_eq!(deferred, vec![warning]);
}

#[test]
fn publication_deferred_follow_up_uses_publish_retry() {
    let follow_up = publication_deferred_follow_up(None, "forced".to_string(), "/tmp/recovery.db");
    assert_eq!(follow_up.kind, "generation_publication");
    assert_eq!(follow_up.status, "pending");
    assert_eq!(
        follow_up.retry_command.as_deref(),
        Some("conary system generation publish --yes --db-path='/tmp/recovery.db'")
    );
}

#[test]
fn no_base_system_deferred_follow_up_drops_the_publish_retry() {
    use crate::commands::generation::publication::PublicationFailureKind;
    use conary_core::MissingBaseSystemPart;
    let follow_up = publication_deferred_follow_up(
        Some(PublicationFailureKind::NoBaseSystem(
            MissingBaseSystemPart::MissingInit,
        )),
        "generation publication is pending".to_string(),
        "/tmp/recovery.db",
    );
    assert_eq!(follow_up.kind, NO_BASE_SYSTEM_MISSING_INIT_KIND);
    assert_eq!(follow_up.status, "pending");
    assert_eq!(follow_up.retry_command, None);
    assert_eq!(
        follow_up.message,
        crate::ui::publication::NO_BASE_SYSTEM_MISSING_INIT_REASON
    );
}

#[test]
fn missing_boot_assets_follow_up_records_the_boot_asset_reason() {
    use crate::commands::generation::publication::PublicationFailureKind;
    use conary_core::MissingBaseSystemPart;
    let follow_up = publication_deferred_follow_up(
        Some(PublicationFailureKind::NoBaseSystem(
            MissingBaseSystemPart::MissingBootAssets,
        )),
        "generation publication is pending".to_string(),
        "/tmp/recovery.db",
    );
    assert_eq!(follow_up.kind, NO_BASE_SYSTEM_MISSING_BOOT_ASSETS_KIND);
    assert_eq!(follow_up.retry_command, None);
    assert_eq!(
        follow_up.message,
        crate::ui::publication::NO_BASE_SYSTEM_MISSING_BOOT_ASSETS_REASON
    );
}

#[test]
fn rejects_superseded_schema_without_fallback() {
    let raw = serde_json::json!({
        "schema": "conary.changeset.metadata.v5",
        "removed_troves": [snapshot("fixture")],
    })
    .to_string();

    let err = parse_rollback_snapshots(&raw).unwrap_err().to_string();

    assert!(err.contains("Unsupported changeset metadata schema"));
    assert!(err.contains("conary.changeset.metadata.v5"));
}

#[test]
fn rollback_authority_sections_are_required_as_one_exact_v7_contract() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conary_core::db::schema::ensure_current(&conn).unwrap();
    let snapshot_id = rollback_root(&conn).id();
    conn.execute(
        "INSERT INTO changesets (
             description, status, rollback_selected_root_snapshot_id
         ) VALUES ('fixture', 'applied', ?1)",
        [snapshot_id],
    )
    .unwrap();
    let changeset_id = conn.last_insert_rowid();
    let complete = metadata_with_removed_troves(
        Vec::new(),
        Vec::new(),
        SelectedRootSnapshot::find(&conn, snapshot_id)
            .unwrap()
            .unwrap(),
        RollbackSystemAuthority::default(),
    )
    .unwrap();
    let complete: serde_json::Value = serde_json::from_str(&complete).unwrap();

    for missing in [
        "rollback_system_authority",
        "rollback_materialized_directories",
    ] {
        let mut incomplete = complete.clone();
        incomplete.as_object_mut().unwrap().remove(missing);
        let error = parse_rollback_authority(&conn, changeset_id, &incomplete.to_string())
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(
                "rollback root, materialized-directory, and native-system authority must be persisted together"
            ),
            "{missing}: {error}"
        );
    }

    let snapshots_without_authority = serde_json::json!({
        "schema": CHANGESET_METADATA_SCHEMA,
        "removed_troves": [snapshot("fixture")],
        "deferred_follow_up": [],
        "adoption_warnings": [],
    });
    let error = parse_rollback_snapshots(&snapshots_without_authority.to_string())
        .unwrap_err()
        .to_string();
    assert!(
        error.contains(
            "trove snapshots require exact pre-mutation selected-root, materialized-directory, and native-system authority"
        ),
        "{error}"
    );
}

#[test]
fn unversioned_or_malformed_metadata_is_rejected() {
    let raw = serde_json::to_string(&snapshot("fixture")).unwrap();

    assert!(parse_rollback_snapshots(&raw).is_err());
    assert!(deferred_follow_up(Some(&raw)).is_err());
    assert!(deferred_follow_up(Some("not-json")).is_err());
    assert!(deferred_follow_up(None).unwrap().is_empty());
}

#[test]
fn append_deferred_follow_up_preserves_removed_troves() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("conary.db");
    conary_core::db::init(&db_path).unwrap();
    let conn = conary_core::db::open(&db_path).unwrap();
    let mut changeset = conary_core::db::models::Changeset::new("Remove fixture".to_string());
    let changeset_id = changeset.insert(&conn).unwrap();
    let rollback_root = rollback_root(&conn);
    let initial = metadata_with_removed_troves(
        vec![snapshot("fixture")],
        Vec::new(),
        rollback_root,
        RollbackSystemAuthority::default(),
    )
    .unwrap();
    conn.execute(
        "UPDATE changesets
         SET metadata = ?1, rollback_selected_root_snapshot_id = ?2
         WHERE id = ?3",
        rusqlite::params![initial, rollback_root.id(), changeset_id],
    )
    .unwrap();

    append_deferred_follow_up_metadata(
        &conn,
        changeset_id,
        DeferredFollowUp {
            kind: "state_snapshot".to_string(),
            status: "failed".to_string(),
            message: "snapshot failed".to_string(),
            retry_command: Some("conary system state create \"Remove fixture\"".to_string()),
        },
    )
    .unwrap();

    let raw: String = conn
        .query_row(
            "SELECT metadata FROM changesets WHERE id = ?1",
            [changeset_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(parse_rollback_snapshots(&raw).unwrap()[0].name, "fixture");
    assert_eq!(deferred_follow_up(Some(&raw)).unwrap().len(), 1);
}

#[test]
fn append_rejects_corrupt_metadata_without_overwriting_it() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("conary.db");
    conary_core::db::init(&db_path).unwrap();
    let conn = conary_core::db::open(&db_path).unwrap();
    let mut changeset = conary_core::db::models::Changeset::new("Corrupt fixture".to_string());
    let changeset_id = changeset.insert(&conn).unwrap();
    conn.execute(
        "UPDATE changesets SET metadata = 'not-json' WHERE id = ?1",
        [changeset_id],
    )
    .unwrap();

    let error = append_deferred_follow_up_metadata(
        &conn,
        changeset_id,
        publication_deferred_follow_up(None, "pending".to_string(), db_path.to_str().unwrap()),
    )
    .expect_err("corrupt persisted metadata must stop the append");
    assert!(error.to_string().contains("expected ident"));

    let raw: String = conn
        .query_row(
            "SELECT metadata FROM changesets WHERE id = ?1",
            [changeset_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(raw, "not-json");
}

#[test]
fn unknown_envelope_fields_are_rejected() {
    let raw = serde_json::json!({
        "schema": CHANGESET_METADATA_SCHEMA,
        "removed_troves": [],
        "deferred_follow_up": [],
        "adoption_warnings": [],
        "invented": true,
    })
    .to_string();

    let error = deferred_follow_up(Some(&raw))
        .expect_err("unknown current-schema fields must not be ignored");
    assert!(error.to_string().contains("unknown field `invented`"));
}
