// apps/conary/src/commands/changeset_metadata.rs

#[cfg(test)]
use super::FileSnapshot;
use super::MaterializedDirectorySnapshot;
use super::RollbackSystemAuthority;
use super::TroveSnapshot;
use anyhow::{Result, bail};
use conary_core::generation::root_manifest::SelectedRootSnapshot;
use serde::{Deserialize, Serialize};

pub(crate) const CHANGESET_METADATA_SCHEMA: &str = "conary.changeset.metadata.v7";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeferredFollowUp {
    pub kind: String,
    pub status: String,
    pub message: String,
    pub retry_command: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeferredFollowUpKind {
    GenerationPublication,
    /// Publication is pending because the selected root has no executable
    /// `/sbin/init`, so re-running publication cannot succeed until a base
    /// system provides one.
    GenerationPublicationNoBaseSystemInit,
    /// Publication is pending because the selected root has no kernel or EFI
    /// boot assets, so re-running publication cannot succeed until they exist.
    GenerationPublicationNoBaseSystemBootAssets,
    Other,
}

/// Persisted follow-up kind when the selected root has no executable
/// `/sbin/init`.
const NO_BASE_SYSTEM_MISSING_INIT_KIND: &str = "generation_publication_no_base_system_missing_init";

/// Persisted follow-up kind when the selected root has no kernel or boot
/// assets.
const NO_BASE_SYSTEM_MISSING_BOOT_ASSETS_KIND: &str =
    "generation_publication_no_base_system_missing_boot_assets";

/// The recorded follow-up kind for the exact missing base-system part.
///
/// The kind is the durable authority the history renderer classifies back into
/// [`DeferredFollowUpKind`]; it must never be reconstructed from the message.
fn no_base_system_follow_up_kind(missing: conary_core::MissingBaseSystemPart) -> &'static str {
    match missing {
        conary_core::MissingBaseSystemPart::MissingInit => NO_BASE_SYSTEM_MISSING_INIT_KIND,
        conary_core::MissingBaseSystemPart::MissingBootAssets => {
            NO_BASE_SYSTEM_MISSING_BOOT_ASSETS_KIND
        }
    }
}

pub(crate) fn classify_deferred_follow_up_kind(
    follow_up: &DeferredFollowUp,
) -> DeferredFollowUpKind {
    match follow_up.kind.as_str() {
        "generation_publication" => DeferredFollowUpKind::GenerationPublication,
        NO_BASE_SYSTEM_MISSING_INIT_KIND => {
            DeferredFollowUpKind::GenerationPublicationNoBaseSystemInit
        }
        NO_BASE_SYSTEM_MISSING_BOOT_ASSETS_KIND => {
            DeferredFollowUpKind::GenerationPublicationNoBaseSystemBootAssets
        }
        _ => DeferredFollowUpKind::Other,
    }
}

pub(crate) fn publication_deferred_follow_up(
    failure_kind: Option<crate::commands::generation::publication::PublicationFailureKind>,
    pending_message: String,
    db_path: &str,
) -> DeferredFollowUp {
    use crate::commands::generation::publication::PublicationFailureKind;
    if let Some(PublicationFailureKind::NoBaseSystem(missing)) = failure_kind {
        return DeferredFollowUp {
            kind: no_base_system_follow_up_kind(missing).to_string(),
            status: "pending".to_string(),
            message: crate::ui::publication::no_base_system_reason(missing).to_string(),
            retry_command: None,
        };
    }
    DeferredFollowUp {
        kind: "generation_publication".to_string(),
        status: "pending".to_string(),
        message: pending_message,
        retry_command: Some(
            super::generation::publication::PublicationOutcome::retry_command(db_path),
        ),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AdoptionWarning {
    pub package: String,
    pub reason: String,
    pub total_inserts: usize,
    pub failed_inserts: usize,
}

impl AdoptionWarning {
    pub(crate) fn refresh_replacement_failure(
        package: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            package: package.into(),
            reason: format!("refresh_replacement_failed: {}", message.into()),
            total_inserts: 0,
            failed_inserts: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangesetMetadataEnvelope {
    pub schema: String,
    #[serde(default)]
    pub removed_troves: Vec<TroveSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback_system_authority: Option<RollbackSystemAuthority>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback_materialized_directories: Option<Vec<MaterializedDirectorySnapshot>>,
    #[serde(default)]
    pub deferred_follow_up: Vec<DeferredFollowUp>,
    #[serde(default)]
    pub adoption_warnings: Vec<AdoptionWarning>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RollbackAuthority {
    pub removed_troves: Vec<TroveSnapshot>,
    pub selected_root: SelectedRootSnapshot,
    pub system: RollbackSystemAuthority,
    pub materialized_directories: Vec<MaterializedDirectorySnapshot>,
}

pub(crate) fn metadata_with_removed_troves(
    snapshots: Vec<TroveSnapshot>,
    rollback_materialized_directories: Vec<MaterializedDirectorySnapshot>,
    _rollback_root: SelectedRootSnapshot,
    rollback_system_authority: RollbackSystemAuthority,
) -> Result<String> {
    metadata_with_envelope_sections(
        snapshots,
        true,
        Some(rollback_system_authority),
        Some(rollback_materialized_directories),
        Vec::new(),
        Vec::new(),
    )
}

#[cfg(test)]
pub(crate) fn metadata_with_deferred_follow_up(
    snapshots: Vec<TroveSnapshot>,
    deferred_follow_up: Vec<DeferredFollowUp>,
) -> Result<String> {
    metadata_with_envelope_sections(snapshots, false, None, None, deferred_follow_up, Vec::new())
}

#[cfg(test)]
pub(crate) fn metadata_with_adoption_warnings(
    snapshots: Vec<TroveSnapshot>,
    deferred_follow_up: Vec<DeferredFollowUp>,
    adoption_warnings: Vec<AdoptionWarning>,
) -> Result<String> {
    metadata_with_envelope_sections(
        snapshots,
        false,
        None,
        None,
        deferred_follow_up,
        adoption_warnings,
    )
}

fn metadata_with_envelope_sections(
    snapshots: Vec<TroveSnapshot>,
    has_rollback_root: bool,
    rollback_system_authority: Option<RollbackSystemAuthority>,
    rollback_materialized_directories: Option<Vec<MaterializedDirectorySnapshot>>,
    deferred_follow_up: Vec<DeferredFollowUp>,
    adoption_warnings: Vec<AdoptionWarning>,
) -> Result<String> {
    validate_rollback_authority_binding(
        &snapshots,
        has_rollback_root,
        rollback_system_authority.as_ref(),
        rollback_materialized_directories.as_deref(),
    )?;
    serde_json::to_string(&ChangesetMetadataEnvelope {
        schema: CHANGESET_METADATA_SCHEMA.to_string(),
        removed_troves: snapshots,
        rollback_system_authority,
        rollback_materialized_directories,
        deferred_follow_up,
        adoption_warnings,
    })
    .map_err(Into::into)
}

#[cfg(test)]
pub(crate) fn parse_rollback_snapshots(snapshot_json: &str) -> Result<Vec<TroveSnapshot>> {
    Ok(parse_changeset_metadata(Some(snapshot_json))?.removed_troves)
}

pub(crate) fn parse_rollback_authority(
    conn: &rusqlite::Connection,
    changeset_id: i64,
    snapshot_json: &str,
) -> Result<Option<RollbackAuthority>> {
    let envelope = parse_changeset_metadata(Some(snapshot_json))?;
    let snapshot_id = conn.query_row(
        "SELECT rollback_selected_root_snapshot_id FROM changesets WHERE id = ?1",
        [changeset_id],
        |row| row.get::<_, Option<i64>>(0),
    )?;
    Ok(
        match (
            snapshot_id,
            envelope.rollback_system_authority,
            envelope.rollback_materialized_directories,
        ) {
            (Some(snapshot_id), Some(system), Some(materialized_directories)) => {
                let selected_root = SelectedRootSnapshot::find(conn, snapshot_id)?.ok_or_else(|| {
                anyhow::anyhow!(
                    "changeset {changeset_id} references missing selected-root snapshot {snapshot_id}"
                )
            })?;
                Some(RollbackAuthority {
                    removed_troves: envelope.removed_troves,
                    selected_root,
                    system,
                    materialized_directories,
                })
            }
            (None, None, None) => None,
            _ => bail!(
                "changeset {changeset_id} rollback snapshot, materialized-directory, and native-system authority must be persisted together"
            ),
        },
    )
}

fn empty_changeset_metadata() -> ChangesetMetadataEnvelope {
    ChangesetMetadataEnvelope {
        schema: CHANGESET_METADATA_SCHEMA.to_string(),
        removed_troves: Vec::new(),
        rollback_system_authority: None,
        rollback_materialized_directories: None,
        deferred_follow_up: Vec::new(),
        adoption_warnings: Vec::new(),
    }
}

fn parse_changeset_metadata(snapshot_json: Option<&str>) -> Result<ChangesetMetadataEnvelope> {
    let Some(snapshot_json) = snapshot_json else {
        return Ok(empty_changeset_metadata());
    };
    let value = serde_json::from_str::<serde_json::Value>(snapshot_json)?;
    let Some(schema) = value.get("schema").and_then(serde_json::Value::as_str) else {
        bail!(
            "Unsupported changeset metadata: missing string schema; expected {CHANGESET_METADATA_SCHEMA}"
        );
    };
    if schema != CHANGESET_METADATA_SCHEMA {
        bail!(
            "Unsupported changeset metadata schema {schema}; expected {CHANGESET_METADATA_SCHEMA}"
        );
    }

    let envelope: ChangesetMetadataEnvelope = serde_json::from_value(value)?;
    validate_rollback_authority_binding(
        &envelope.removed_troves,
        envelope.rollback_system_authority.is_some(),
        envelope.rollback_system_authority.as_ref(),
        envelope.rollback_materialized_directories.as_deref(),
    )?;
    Ok(envelope)
}

pub(crate) fn deferred_follow_up(snapshot_json: Option<&str>) -> Result<Vec<DeferredFollowUp>> {
    Ok(parse_changeset_metadata(snapshot_json)?.deferred_follow_up)
}

#[cfg(test)]
pub(crate) fn adoption_warnings(snapshot_json: Option<&str>) -> Result<Vec<AdoptionWarning>> {
    Ok(parse_changeset_metadata(snapshot_json)?.adoption_warnings)
}

pub(crate) fn append_deferred_follow_up_metadata(
    conn: &rusqlite::Connection,
    changeset_id: i64,
    follow_up: DeferredFollowUp,
) -> Result<()> {
    let existing: Option<String> = conn.query_row(
        "SELECT metadata FROM changesets WHERE id = ?1",
        [changeset_id],
        |row| row.get(0),
    )?;
    let mut envelope = parse_changeset_metadata(existing.as_deref())?;
    envelope.deferred_follow_up.push(follow_up);
    let metadata = metadata_with_envelope_sections(
        envelope.removed_troves,
        envelope.rollback_system_authority.is_some(),
        envelope.rollback_system_authority,
        envelope.rollback_materialized_directories,
        envelope.deferred_follow_up,
        envelope.adoption_warnings,
    )?;
    conn.execute(
        "UPDATE changesets SET metadata = ?1 WHERE id = ?2",
        rusqlite::params![metadata, changeset_id],
    )?;
    Ok(())
}

pub(crate) fn append_adoption_warning_metadata(
    conn: &rusqlite::Connection,
    changeset_id: i64,
    warnings: Vec<AdoptionWarning>,
) -> Result<()> {
    if warnings.is_empty() {
        return Ok(());
    }

    let existing: Option<String> = conn.query_row(
        "SELECT metadata FROM changesets WHERE id = ?1",
        [changeset_id],
        |row| row.get(0),
    )?;
    let mut envelope = parse_changeset_metadata(existing.as_deref())?;
    envelope.adoption_warnings.extend(warnings);
    let metadata = metadata_with_envelope_sections(
        envelope.removed_troves,
        envelope.rollback_system_authority.is_some(),
        envelope.rollback_system_authority,
        envelope.rollback_materialized_directories,
        envelope.deferred_follow_up,
        envelope.adoption_warnings,
    )?;
    conn.execute(
        "UPDATE changesets SET metadata = ?1 WHERE id = ?2",
        rusqlite::params![metadata, changeset_id],
    )?;
    Ok(())
}

fn validate_rollback_authority_binding(
    snapshots: &[TroveSnapshot],
    has_rollback_root: bool,
    rollback_system_authority: Option<&RollbackSystemAuthority>,
    rollback_materialized_directories: Option<&[MaterializedDirectorySnapshot]>,
) -> Result<()> {
    if !snapshots.is_empty()
        && (!has_rollback_root
            || rollback_system_authority.is_none()
            || rollback_materialized_directories.is_none())
    {
        bail!(
            "changeset rollback trove snapshots require exact pre-mutation selected-root, materialized-directory, and native-system authority"
        );
    }
    if has_rollback_root != rollback_system_authority.is_some()
        || has_rollback_root != rollback_materialized_directories.is_some()
    {
        bail!(
            "changeset rollback root, materialized-directory, and native-system authority must be persisted together"
        );
    }
    if let Some(system) = rollback_system_authority {
        system.validate()?;
    }
    if let Some(directories) = rollback_materialized_directories {
        let mut paths = std::collections::BTreeSet::new();
        for directory in directories {
            directory.validate()?;
            if !paths.insert(directory.path.as_str()) {
                bail!(
                    "changeset rollback authority repeats materialized directory '{}'",
                    directory.path
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "changeset_metadata/tests.rs"]
mod tests;
