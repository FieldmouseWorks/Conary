// apps/conary-test/src/explorer/selected_state.rs

//! Read the product's existing snapshot authority and independently hash its bytes.
//! No materialization, package mutation, or second package-state engine lives here.
use super::contract::{Facts, Package, PublicationFact};
use anyhow::{Result, ensure};
use conary_core::db::models::GenerationPublication;
use conary_core::generation::root_manifest::SelectedRootSnapshot;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use sha2::{Digest, Sha256};
use std::path::Path;

pub fn observe(runtime: &Path) -> Result<Facts> {
    let mut connection =
        Connection::open_with_flags(runtime.join("conary.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    // One read transaction pins package, ownership and snapshot observations to
    // the same database state, without repeated container exec/API round trips.
    let conn = connection.transaction()?;
    let mut facts = Facts {
        packages: Default::default(),
        owners: Default::default(),
        payloads: Default::default(),
        publication: None,
        complete: false,
    };
    for package in [Package::App, Package::Companion] {
        for (sql, parameter, target) in [
            (
                "SELECT version FROM troves WHERE type='package' AND name=?1 LIMIT 2",
                package.name().to_owned(),
                &mut facts.packages,
            ),
            (
                "SELECT t.name FROM files f JOIN troves t ON t.id=f.trove_id WHERE f.path=?1 LIMIT 2",
                package.path(),
                &mut facts.owners,
            ),
        ] {
            let mut statement = conn.prepare(sql)?;
            let values = statement
                .query_map([parameter], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            ensure!(
                values.len() <= 1,
                "ambiguous fixture package/owner evidence"
            );
            if let Some(value) = values.into_iter().next() {
                target.insert(package, value);
            }
        }
    }
    let id = conn.query_row(
        "SELECT id FROM generation_publications WHERE status != 'abandoned' ORDER BY id DESC LIMIT 1",
        [], |row| row.get::<_, i64>(0),
    ).optional()?;
    let Some(id) = id else {
        ensure!(
            facts.packages.is_empty() && facts.owners.is_empty(),
            "package state lacks selected-root publication authority"
        );
        facts.complete = true;
        return Ok(facts);
    };
    let publication = GenerationPublication::find_by_id(&conn, id)?
        .ok_or_else(|| anyhow::anyhow!("missing publication"))?;
    let snapshot_id = publication
        .selected_root_snapshot_id
        .ok_or_else(|| anyhow::anyhow!("missing selected-root snapshot identity"))?;
    let snapshot = SelectedRootSnapshot::find(&conn, snapshot_id)?
        .ok_or_else(|| anyhow::anyhow!("missing selected-root snapshot"))?;
    facts.publication = Some(PublicationFact {
        snapshot_id,
        status: publication.status.as_str().into(),
        phase: publication.phase.as_str().into(),
        error: publication.last_error,
    });
    for package in [Package::App, Package::Companion] {
        if let Some(entry) = snapshot.entry(&conn, &package.path())? {
            entry.validate()?;
            let content = entry
                .content
                .ok_or_else(|| anyhow::anyhow!("fixture payload is not a regular file"))?;
            ensure!(
                content.size <= 65536,
                "fixture content exceeds observation limit"
            );
            let path =
                conary_core::filesystem::object_path(&runtime.join("objects"), &content.sha256)?;
            let metadata = std::fs::symlink_metadata(&path)?;
            ensure!(
                metadata.is_file() && metadata.len() == content.size,
                "fixture CAS size/type mismatch"
            );
            let bytes = std::fs::read(path)?;
            let actual = hex::encode(Sha256::digest(&bytes));
            ensure!(
                actual == content.sha256,
                "selected-root CAS digest mismatch"
            );
            facts.payloads.insert(package, actual);
        }
    }
    facts.complete = true;
    Ok(facts)
}
