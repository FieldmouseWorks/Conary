// apps/remi/src/server/native_oracle_input/retention.rs

//! Export-owned catalog reachability across independent native producer jobs.
//!
//! Existing durable work pins retain bytes, never candidate or promotion authority.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, ensure};
use conary_core::db::models::{RemiProfileRevisionPin, RemiRevisionPinKind};
use rusqlite::{Connection, Transaction, TransactionBehavior};
use serde::Serialize;

use super::{
    NativeOracleInputSetV1, capture_candidates_from_connection, open_runtime_db,
    reopen_native_oracle_input_bundle, require_unchanged_candidates,
};
use crate::server::catalog_authority::{CatalogAuthority, ProfileRevisionSelection};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NativeOracleInputRetention {
    pub schema_version: u32,
    pub export_id: String,
    pub input_manifest_sha256: String,
    pub profiles: Vec<NativeOracleInputRetainedProfile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NativeOracleInputRetainedProfile {
    pub profile: String,
    pub profile_revision_sha256: String,
    pub packages: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NativeOracleInputRelease {
    pub schema_version: u32,
    pub export_id: String,
    pub input_manifest_sha256: String,
    pub released_profiles: usize,
}

pub(super) fn validate_export_id(export_id: &str) -> Result<()> {
    ensure!(
        !export_id.is_empty()
            && export_id.len() <= 128
            && (export_id.as_bytes()[0].is_ascii_lowercase()
                || export_id.as_bytes()[0].is_ascii_digit())
            && export_id.bytes().all(|byte| byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || b"._-".contains(&byte)),
        "native-oracle export ID must be a bounded plain identity"
    );
    Ok(())
}

fn owner_prefix(export_id: &str, manifest_sha256: &str) -> Result<String> {
    validate_export_id(export_id)?;
    super::validate_sha256(manifest_sha256)?;
    Ok(format!(
        "native-oracle-input-v1:{export_id}:{manifest_sha256}:"
    ))
}

fn owner(export_id: &str, manifest_sha256: &str, profiles: usize) -> Result<String> {
    ensure!(
        profiles > 0,
        "native-oracle retention set must not be empty"
    );
    let profiles =
        u32::try_from(profiles).context("native-oracle retention profile count overflow")?;
    Ok(format!(
        "{}{profiles}",
        owner_prefix(export_id, manifest_sha256)?
    ))
}

pub(super) fn require_unused_export(db_path: &Path, export_id: &str) -> Result<()> {
    validate_export_id(export_id)?;
    let conn = open_runtime_db(db_path)?;
    let owned = RemiProfileRevisionPin::discover_owner_prefix(
        &conn,
        RemiRevisionPinKind::Work,
        &format!("native-oracle-input-v1:{export_id}:"),
    )?;
    ensure!(
        owned.is_empty(),
        "native-oracle export identity already owns retained catalogs; use a new export identity"
    );
    Ok(())
}

fn pin_id(export_id: &str, profile: &str) -> String {
    // The fixed namespace and canonical profile ID delimit the caller identity.
    conary_core::hash::sha256(format!("native-oracle-input-v1:{export_id}:{profile}").as_bytes())
}

fn receipt(
    export_id: &str,
    manifest: &NativeOracleInputSetV1,
) -> Result<NativeOracleInputRetention> {
    validate_export_id(export_id)?;
    super::validate_manifest(manifest)?;
    Ok(NativeOracleInputRetention {
        schema_version: 1,
        export_id: export_id.to_string(),
        input_manifest_sha256: conary_core::hash::sha256(
            &conary_core::json::canonical_json(manifest).map_err(anyhow::Error::msg)?,
        ),
        profiles: manifest
            .profiles
            .iter()
            .map(|profile| NativeOracleInputRetainedProfile {
                profile: profile.revision.profile.clone(),
                profile_revision_sha256: profile.profile_revision_sha256.clone(),
                packages: profile.revision.counts.packages,
            })
            .collect(),
    })
}

pub(super) fn retain_export(
    db_path: &Path,
    export_id: &str,
    manifest: &NativeOracleInputSetV1,
    initial: &[conary_core::repository::ProfileSyncCandidate],
) -> Result<NativeOracleInputRetention> {
    let retained = receipt(export_id, manifest)?;
    let owner_identity = owner(
        export_id,
        &retained.input_manifest_sha256,
        retained.profiles.len(),
    )?;
    let selections = retained.selections();
    let pinned_at = i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())?;
    let conn = open_runtime_db(db_path)?;
    let tx = Transaction::new_unchecked(&conn, TransactionBehavior::Immediate)?;
    // Reader pins remain held by the exporter throughout this transaction.
    let current = capture_candidates_from_connection(&tx, &selections)?;
    require_unchanged_candidates(initial, &current)?;
    for profile in &retained.profiles {
        RemiProfileRevisionPin {
            pin_id: pin_id(export_id, &profile.profile),
            source_profile: profile.profile.clone(),
            profile_revision_sha256: profile.profile_revision_sha256.clone(),
            owner_kind: RemiRevisionPinKind::Work,
            owner_identity: owner_identity.clone(),
            runtime_session_id: None,
            pinned_at,
        }
        .insert(&tx)
        .context("retain complete export-owned native-oracle catalog set")?;
    }
    tx.commit()
        .context("commit native-oracle export retention")?;
    Ok(retained)
}

impl NativeOracleInputRetention {
    pub(crate) fn selections(&self) -> Vec<ProfileRevisionSelection> {
        self.profiles
            .iter()
            .map(|profile| ProfileRevisionSelection {
                source_profile: profile.profile.clone(),
                profile_revision_sha256: profile.profile_revision_sha256.clone(),
            })
            .collect()
    }
}

fn require_pin_set(conn: &Connection, retained: &NativeOracleInputRetention) -> Result<()> {
    let discovered = RemiProfileRevisionPin::discover_owner_prefix(
        conn,
        RemiRevisionPinKind::Work,
        &owner_prefix(&retained.export_id, &retained.input_manifest_sha256)?,
    )?;
    ensure!(
        discovered.len() == retained.profiles.len(),
        "native-oracle export retention is absent or released, partial, or has unexpected members; rebuild the export"
    );
    let owner_identity = owner(
        &retained.export_id,
        &retained.input_manifest_sha256,
        retained.profiles.len(),
    )?;
    for profile in &retained.profiles {
        let pin =
            RemiProfileRevisionPin::find(conn, &pin_id(&retained.export_id, &profile.profile))?
                .context(
                    "native-oracle export retention is absent or released; rebuild the export",
                )?;
        ensure!(
            pin.source_profile == profile.profile
                && pin.profile_revision_sha256 == profile.profile_revision_sha256
                && pin.owner_kind == RemiRevisionPinKind::Work
                && pin.owner_identity == owner_identity
                && pin.runtime_session_id.is_none(),
            "native-oracle export retention does not match its exact owner and revision"
        );
    }
    Ok(())
}

/// Independently authenticate an export's retained immutable catalog set.
/// Current candidate pointers are deliberately not consulted: this is diagnostic
/// input retention, and cannot satisfy promotion's current-candidate predicate.
pub fn inspect_native_oracle_input_retention(
    db_path: &Path,
    catalog_dir: &Path,
    input_dir: &Path,
    export_id: &str,
) -> Result<NativeOracleInputRetention> {
    validate_export_id(export_id)?;
    let manifest = reopen_native_oracle_input_bundle(input_dir)?;
    let retained = receipt(export_id, &manifest)?;
    let conn = open_runtime_db(db_path)?;
    require_pin_set(&conn, &retained)?;
    let authority = CatalogAuthority::for_inspection(db_path, catalog_dir);
    // Pin the entire set before any slow full reopen. Release and GC cannot
    // remove a later profile while the first profile is being authenticated.
    let selections = retained.selections();
    let pins = authority.open_selected_profiles(&selections)?;
    for ((selection, pin), expected) in selections.iter().zip(&pins).zip(&manifest.profiles) {
        let inspected = authority.verify_selected_profile_complete(selection)?;
        ensure!(
            inspected.manifest == expected.revision && pin.manifest() == &expected.revision,
            "retained native-oracle profile differs from its registered catalog"
        );
        for (ordinal, source) in expected.sources.iter().enumerate() {
            let actual = authority.source_bundle_for_member(pin, ordinal as u32)?;
            ensure!(
                actual.manifest == *source,
                "retained native-oracle source identity changed"
            );
        }
    }
    require_pin_set(&conn, &retained)?;
    Ok(retained)
}

/// Release exactly one complete export-owned set, even if its artifact bytes
/// have been lost. The caller supplies the authenticated manifest identity;
/// neither current pointers nor another owner's pins authorize this mutation.
pub fn release_native_oracle_input_retention(
    db_path: &Path,
    export_id: &str,
    input_manifest_sha256: &str,
) -> Result<NativeOracleInputRelease> {
    let prefix = owner_prefix(export_id, input_manifest_sha256)?;
    let conn = open_runtime_db(db_path)?;
    let tx = Transaction::new_unchecked(&conn, TransactionBehavior::Immediate)?;
    let pins =
        RemiProfileRevisionPin::discover_owner_prefix(&tx, RemiRevisionPinKind::Work, &prefix)?;
    let first = pins
        .first()
        .context("native-oracle export retention is absent, partial, or already released")?;
    let count = first
        .owner_identity
        .strip_prefix(&prefix)
        .context("native-oracle retention owner namespace changed")?
        .parse::<u32>()
        .context("invalid native-oracle retention set count")?;
    let owner_identity = owner(export_id, input_manifest_sha256, usize::try_from(count)?)?;
    ensure!(
        pins.len() == usize::try_from(count)?,
        "native-oracle export retention is partial or has unexpected members"
    );
    let mut profiles = std::collections::BTreeSet::new();
    for pin in &pins {
        ensure!(
            pin.pin_id == pin_id(export_id, &pin.source_profile)
                && pin.owner_kind == RemiRevisionPinKind::Work
                && pin.owner_identity == owner_identity
                && pin.runtime_session_id.is_none()
                && profiles.insert(pin.source_profile.clone()),
            "native-oracle release does not match its exact export owner"
        );
        ensure!(
            RemiProfileRevisionPin::release(&tx, &pin.pin_id)?,
            "native-oracle pin disappeared during release"
        );
    }
    tx.commit()
        .context("commit complete native-oracle retention release")?;
    Ok(NativeOracleInputRelease {
        schema_version: 1,
        export_id: export_id.to_string(),
        input_manifest_sha256: input_manifest_sha256.to_string(),
        released_profiles: pins.len(),
    })
}

#[cfg(test)]
mod tests;
