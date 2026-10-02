// apps/conary-test/src/config/tests/native_corpus/typed_regular/fixtures.rs
#![cfg(test)]

use super::super::super::conary_fixture_path;
use conary_core::payload::{PayloadNode, PayloadNodeKind, ResolvedPayloadNode};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

pub(super) const ANCHOR: &str = "/usr/lib/phase4-corpus/hardlink-anchor";
pub(super) const COPY: &str = "/usr/lib/phase4-corpus/hardlink-copy";
pub(super) const HARDLINK_SHA: &str =
    "7c1e048a51efb4f45a2910924eac0cd847852cae01e252c0c1a9759d9d07480d";
const ZERO_SHA: &str = "5647f05ec18958947d32874eeb788fa396a05d0bab7c1b71f112ceb7e9b31eee";
pub(super) const TRACKED: &[(&str, i64, &str)] = &[
    (
        "/etc/phase4-corpus/app-deleted.conf",
        22,
        "d9697bd8e59efc7dc9863c9ae9b0c3745262015b3552a20712a9bcbfc3e47f79",
    ),
    (
        "/etc/phase4-corpus/app-local.conf",
        20,
        "9773fc86d4e388ba3f5eba7408708b46ce20738acf98fc55a4e5f584bbe8af66",
    ),
    (
        "/etc/phase4-corpus/app.conf",
        48,
        "8b3ca388884ac9c33e5e1c849ac30bc59aa9d494e7afc5d91fb56c8a4e760eb2",
    ),
    (
        "/usr/bin/phase4-corpus",
        48,
        "565d3f61ffdbbca53c7371588ce6884b680dfa1d1c1bd7fe3954b4ab957e0676",
    ),
    (
        "/usr/bin/phase4-corpus-alt",
        52,
        "1c279f8af2faaed5ae4aad470ab37742b2cede45b42b65d961f2bb33233e5ad3",
    ),
    (
        "/usr/lib/kernel/install.d/95-phase4-corpus.install",
        96,
        "8753356f470ea82416c2d7a71cbf8367d4cbe1a8b78f3276fe45035e114c3674",
    ),
    (
        "/usr/lib/systemd/system/phase4-corpus.service",
        137,
        "20200a86933288373c109a2e3289b2314ff6015886bfbad9bec6e613421e756a",
    ),
];

pub(super) fn verify_pinned_fixture_bytes() {
    for &(path, size, sha) in TRACKED {
        let bytes = std::fs::read(conary_fixture_path(&format!(
            "phase4-daily-driver-corpus/stage{path}"
        )))
        .unwrap_or_else(|error| panic!("read {path}: {error}"));
        assert_eq!(bytes.len() as i64, size, "{path} pinned size");
        assert_eq!(
            conary_core::hash::sha256(&bytes),
            sha,
            "{path} pinned digest"
        );
    }
    let hardlink = b"shared hardlink corpus payload\n";
    assert_eq!(hardlink.len(), 31);
    assert_eq!(conary_core::hash::sha256(hardlink), HARDLINK_SHA);
    let zero = vec![0_u8; 2_097_152];
    assert_eq!(conary_core::hash::sha256(&zero), ZERO_SHA);
}

pub(super) fn expected_rows(regular_hardlink: &str) -> Value {
    let mut rows: Vec<Value> = TRACKED
        .iter()
        .map(|&(path, size, sha)| row(path, size, sha))
        .collect();
    rows.push(row(regular_hardlink, 31, HARDLINK_SHA));
    rows.push(row(
        "/usr/share/phase4-corpus/large-payload.bin",
        2_097_152,
        ZERO_SHA,
    ));
    rows.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    Value::Array(rows)
}

fn row(path: &str, size: i64, sha: &str) -> Value {
    json!({"path": path, "kind": "regular", "content_size": size, "content_sha256": sha})
}

pub(super) fn database(regular_hardlink: &str) -> (tempfile::TempDir, Connection) {
    let directory = tempfile::tempdir().expect("create production-schema witness directory");
    let path = directory.path().join("state.db");
    conary_core::db::init(&path).expect("initialize current production schema");
    let db = conary_core::db::open(&path).expect("open configured production database");
    db.execute_batch("PRAGMA foreign_keys = ON;
        INSERT INTO troves (id, name, version, type, install_source, install_reason, version_scheme)
        VALUES (1, 'phase4-daily-driver-corpus', '1.0.0-1', 'package', 'file', 'explicit', 'conary'),
               (2, 'unrelated-package', '1.0.0-1', 'package', 'file', 'explicit', 'conary');")
        .expect("insert scoped and unrelated troves");
    for (path, size, sha) in TRACKED.iter().copied().chain([
        (regular_hardlink, 31, HARDLINK_SHA),
        (
            "/usr/share/phase4-corpus/large-payload.bin",
            2_097_152,
            ZERO_SHA,
        ),
    ]) {
        insert_file(&db, 1, path, "regular", Some((size, sha)), None);
    }
    let alias = if regular_hardlink == ANCHOR {
        COPY
    } else {
        ANCHOR
    };
    insert_file(&db, 1, alias, "hardlink", None, Some(regular_hardlink));
    insert_file(
        &db,
        2,
        "/usr/share/unrelated-package/file",
        "regular",
        Some((4, HARDLINK_SHA)),
        None,
    );
    (directory, db)
}

pub(super) fn insert_file(
    db: &Connection,
    trove_id: i64,
    path: &str,
    kind: &str,
    content: Option<(i64, &str)>,
    target: Option<&str>,
) {
    let mut source = PayloadNode::regular(0o644);
    if kind == "hardlink" {
        source.kind = PayloadNodeKind::Hardlink {
            target: target.expect("hardlink target").into(),
            identity: "fixture:hardlink".into(),
        };
    }
    let node = ResolvedPayloadNode::from_numeric_source(source).expect("resolve fixture node");
    let payload = serde_json::to_string(&node).expect("serialize typed payload node");
    let (size, sha) = content.map_or((None, None), |(size, sha)| (Some(size), Some(sha)));
    db.execute("INSERT INTO files (path, payload_node_json, content_size, content_sha256, trove_id) VALUES (?1, ?2, ?3, ?4, ?5)", params![path, payload, size, sha, trove_id])
        .expect("insert production file row");
}
