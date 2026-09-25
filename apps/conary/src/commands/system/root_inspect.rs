// apps/conary/src/commands/system/root_inspect.rs

//! Read-only inspection of the committed selected-root node at one path.
//!
//! The authority is the same typed selection the install path prepares: the
//! newest committed selected-root publication snapshot, then the current
//! generation artifact, then the installed database projection, and finally an
//! explicit no-committed-root state. This module looks up one exact path in the
//! selected manifests and never derives a second projection.
//!
//! A database projection has no committed manifest root: it synthesizes `/`
//! from an empty materialization stand-in whose mode and ownership belong to
//! the inspecting process. That one node is reported with
//! [`RootInspectMetadata::Synthesized`] and with its mode, ownership, and
//! content authority withheld; every node read from a committed manifest
//! reports [`RootInspectMetadata::Recorded`].
//!
//! Boot recovery can point `/current` at a valid generation with no state or
//! publication row. A stable link across the read is accepted as
//! [`RootInspectSource::CurrentGeneration`] with
//! [`RootInspectData::recovered_without_state`] set, so callers can tell that
//! the unknown IDs were never recorded rather than simply omitted. An active
//! or orphaned try session that owns the same generation is an uncommitted
//! trial instead, so the read refuses it before recovery is considered.

use anyhow::{Context, Result, bail};
use conary_agent_contract::{InspectResult, OperationEnvelope, OperationStatus, RiskLevel};
use conary_core::generation::root_manifest::CapturedSelectedRoot;
use conary_core::payload::{
    PayloadContentAuthority, PayloadIdentity, PayloadNodeKind, ResolvedPayloadNode,
};
use conary_core::runtime_root::ConaryRuntimeRoot;
use serde::{Deserialize, Serialize};

use crate::commands::generation::selected_root::{
    SelectedRootSource, read_selected_root_baseline_with_source,
};

pub(crate) const ROOT_INSPECT_SCHEMA_VERSION: u32 = 2;

/// Where the reported node's authority came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RootInspectSource {
    PendingSnapshot,
    CurrentGeneration,
    DatabaseProjection,
    NoCommittedRoot,
}

/// The manifest that owns one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RootManifestKind {
    Root,
    MutableState,
}

/// The exact POSIX node kind recorded by the manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RootNodeKind {
    Regular,
    Directory,
    Symlink,
    Hardlink,
    Fifo,
    Socket,
    BlockDevice,
    CharacterDevice,
}

/// Where the reported node field values came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RootInspectMetadata {
    /// The values came from a committed manifest.
    Recorded,
    /// The values describe an inspecting-process stand-in rather than the
    /// committed root, so node metadata is withheld.
    Synthesized,
}

impl RootInspectSource {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::PendingSnapshot => "pending_snapshot",
            Self::CurrentGeneration => "current_generation",
            Self::DatabaseProjection => "database_projection",
            Self::NoCommittedRoot => "no_committed_root",
        }
    }
}

impl RootManifestKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::MutableState => "mutable_state",
        }
    }
}

impl RootNodeKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Regular => "regular",
            Self::Directory => "directory",
            Self::Symlink => "symlink",
            Self::Hardlink => "hardlink",
            Self::Fifo => "fifo",
            Self::Socket => "socket",
            Self::BlockDevice => "block_device",
            Self::CharacterDevice => "character_device",
        }
    }
}

impl RootInspectMetadata {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Recorded => "recorded",
            Self::Synthesized => "synthesized",
        }
    }
}

/// Versioned `data` payload for `system.root.inspect`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct RootInspectData {
    pub(crate) schema_version: u32,
    pub(crate) snapshot_id: Option<i64>,
    pub(crate) changeset_id: Option<i64>,
    /// True when the baseline came from a stable `/current` generation the
    /// pinned snapshot never recorded and no open try session claims, so the
    /// IDs are unknown rather than merely absent. See
    /// `read_selected_root_baseline_with_source`.
    pub(crate) recovered_without_state: bool,
    pub(crate) source: RootInspectSource,
    pub(crate) path: String,
    pub(crate) present: bool,
    pub(crate) manifest: Option<RootManifestKind>,
    pub(crate) metadata: RootInspectMetadata,
    pub(crate) kind: Option<RootNodeKind>,
    pub(crate) mode: Option<u32>,
    pub(crate) uid: Option<u64>,
    pub(crate) gid: Option<u64>,
    pub(crate) user: Option<String>,
    pub(crate) group: Option<String>,
    pub(crate) sha256: Option<String>,
    pub(crate) symlink_target: Option<String>,
    pub(crate) hardlink_target: Option<String>,
}

impl RootInspectData {
    /// Human-facing label for the node metadata authority.
    pub(crate) const fn metadata_label(&self) -> &'static str {
        self.metadata.as_str()
    }
}

/// Open the selected database read-only and render the result as typed JSON or
/// human text.
///
/// Inspection validates the current schema through
/// [`conary_core::db::open_live_read_only`], which creates nothing and takes
/// normal read-only locks so a live WAL snapshot is read atomically while
/// concurrent writers are active. An empty, non-Conary, or retired-schema file
/// is the existing typed rebuild refusal and is left untouched; a readable but
/// non-writable current-schema database inspects normally.
pub fn cmd_root_inspect(db_path: &str, path: &str, json: bool) -> Result<()> {
    let conn = conary_core::db::open_live_read_only(db_path)
        .context("open the current Conary database read-only for root inspection")?;
    let runtime_root = ConaryRuntimeRoot::from_db_path(std::path::PathBuf::from(db_path));
    let data = root_inspect_data(&conn, &runtime_root, path)?;
    if json {
        let result = inspect_result(&data)?;
        crate::ui::message(&serde_json::to_string_pretty(&result)?);
        return Ok(());
    }
    crate::ui::root_inspect::render(&data);
    Ok(())
}

/// Build the versioned inspection payload without emitting it.
pub(crate) fn root_inspect_data(
    conn: &rusqlite::Connection,
    runtime_root: &ConaryRuntimeRoot,
    path: &str,
) -> Result<RootInspectData> {
    let normalized = normalize_lookup_path(path)?;
    // The read-only baseline is the same typed selection a real install makes.
    // Only its database-projection branch creates the private empty
    // materialization stand-in it reads for root metadata and package-unclaimed
    // parent closure; artifact- and snapshot-backed reads need no temp write
    // access. Because that stand-in is synthesized rather than committed, `/`
    // withholds it through `apply_synthesized_root`.
    let (source, captured) = read_selected_root_baseline_with_source(conn, runtime_root)?;
    let reported = report_source(source);

    let mut data = RootInspectData {
        schema_version: ROOT_INSPECT_SCHEMA_VERSION,
        snapshot_id: reported.snapshot_id,
        changeset_id: reported.changeset_id,
        recovered_without_state: reported.recovered_without_state,
        source: reported.source,
        path: normalized.clone(),
        present: false,
        manifest: None,
        metadata: RootInspectMetadata::Recorded,
        kind: None,
        mode: None,
        uid: None,
        gid: None,
        user: None,
        group: None,
        sha256: None,
        symlink_target: None,
        hardlink_target: None,
    };

    // With no committed root nothing is present, not even the empty
    // projection's synthesized `/` node.
    if data.source != RootInspectSource::NoCommittedRoot
        && let Some((manifest, node, content)) = find_captured_node(&captured, &normalized)
    {
        if data.source == RootInspectSource::DatabaseProjection && normalized == "/" {
            apply_synthesized_root(&mut data);
        } else {
            apply_node(&mut data, manifest, node, content);
        }
    }

    Ok(data)
}

/// Serialized report fields selected from one typed baseline source.
struct ReportedSource {
    source: RootInspectSource,
    snapshot_id: Option<i64>,
    changeset_id: Option<i64>,
    recovered_without_state: bool,
}

/// Map the typed selection source onto its serialized report fields.
fn report_source(source: SelectedRootSource) -> ReportedSource {
    match source {
        SelectedRootSource::PendingSnapshot {
            snapshot_id,
            changeset_id,
        } => ReportedSource {
            source: RootInspectSource::PendingSnapshot,
            snapshot_id: Some(snapshot_id),
            changeset_id,
            recovered_without_state: false,
        },
        SelectedRootSource::CurrentGeneration {
            snapshot_id,
            changeset_id,
            recovered_without_state,
        } => ReportedSource {
            source: RootInspectSource::CurrentGeneration,
            snapshot_id,
            changeset_id,
            recovered_without_state,
        },
        SelectedRootSource::DatabaseProjection { changeset_id } => ReportedSource {
            source: RootInspectSource::DatabaseProjection,
            snapshot_id: None,
            changeset_id,
            recovered_without_state: false,
        },
        SelectedRootSource::NoCommittedRoot => ReportedSource {
            source: RootInspectSource::NoCommittedRoot,
            snapshot_id: None,
            changeset_id: None,
            recovered_without_state: false,
        },
    }
}

/// Project one inspection into the shared agent-contract result.
pub(crate) fn inspect_result(data: &RootInspectData) -> Result<InspectResult> {
    let summary = if data.present {
        format!(
            "{} is present in the committed selected root as {}",
            data.path,
            data.kind.map_or("a node", RootNodeKind::as_str)
        )
    } else {
        format!(
            "{} is not present in the committed selected root",
            data.path
        )
    };
    let envelope = OperationEnvelope::new(
        "system.root.inspect",
        OperationStatus::Ok,
        RiskLevel::ReadOnly,
        summary,
    );
    Ok(InspectResult::new(envelope).with_data(serde_json::to_value(data)?))
}

fn find_captured_node<'a>(
    captured: &'a CapturedSelectedRoot,
    path: &str,
) -> Option<(
    RootManifestKind,
    &'a ResolvedPayloadNode,
    Option<&'a PayloadContentAuthority>,
)> {
    if path == "/" {
        return Some((RootManifestKind::Root, &captured.generation.root, None));
    }
    let generation = captured
        .generation
        .entries
        .iter()
        .find(|entry| entry.path == path);
    if let Some(entry) = generation {
        return Some((RootManifestKind::Root, &entry.node, entry.content.as_ref()));
    }
    let state = captured
        .state
        .entries
        .iter()
        .find(|entry| entry.path == path);
    state.map(|entry| {
        (
            RootManifestKind::MutableState,
            &entry.node,
            entry.content.as_ref(),
        )
    })
}

fn apply_node(
    data: &mut RootInspectData,
    manifest: RootManifestKind,
    node: &ResolvedPayloadNode,
    content: Option<&PayloadContentAuthority>,
) {
    data.present = true;
    data.manifest = Some(manifest);
    data.kind = Some(node_kind(&node.source.kind));
    data.mode = Some(node.source.mode & 0o7777);
    data.uid = Some(node.uid);
    data.gid = Some(node.gid);
    data.user = identity_name(&node.source.user);
    data.group = identity_name(&node.source.group);
    data.sha256 = content.map(|content| content.sha256.clone());
    match &node.source.kind {
        PayloadNodeKind::Symlink { target } => data.symlink_target = Some(target.clone()),
        PayloadNodeKind::Hardlink { target, .. } => data.hardlink_target = Some(target.clone()),
        _ => {}
    }
}

/// Report the projection's stand-in `/` node without its ambient metadata.
///
/// The database projection has no committed manifest root; it synthesizes one
/// from the empty materialization destination, whose mode and ownership belong
/// to the inspecting process. The node is present as a directory, but mode,
/// ownership, and content authority are withheld and the record is marked
/// synthesized so no caller mistakes them for the committed root.
fn apply_synthesized_root(data: &mut RootInspectData) {
    data.present = true;
    data.metadata = RootInspectMetadata::Synthesized;
    data.manifest = Some(RootManifestKind::Root);
    data.kind = Some(RootNodeKind::Directory);
}

/// Normalize one lookup path lexically without resolving the filesystem.
fn normalize_lookup_path(path: &str) -> Result<String> {
    if path.is_empty() {
        bail!("selected-root inspect path must not be empty");
    }
    let mut components = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => bail!(
                "selected-root inspect path {path:?} contains '..'; inspect the exact committed path"
            ),
            other => components.push(other),
        }
    }
    Ok(format!("/{}", components.join("/")))
}

fn node_kind(kind: &PayloadNodeKind) -> RootNodeKind {
    match kind {
        PayloadNodeKind::Regular { .. } => RootNodeKind::Regular,
        PayloadNodeKind::Directory => RootNodeKind::Directory,
        PayloadNodeKind::Symlink { .. } => RootNodeKind::Symlink,
        PayloadNodeKind::Hardlink { .. } => RootNodeKind::Hardlink,
        PayloadNodeKind::Fifo => RootNodeKind::Fifo,
        PayloadNodeKind::Socket => RootNodeKind::Socket,
        PayloadNodeKind::BlockDevice { .. } => RootNodeKind::BlockDevice,
        PayloadNodeKind::CharacterDevice { .. } => RootNodeKind::CharacterDevice,
    }
}

fn identity_name(identity: &PayloadIdentity) -> Option<String> {
    match identity {
        PayloadIdentity::Numeric { .. } => None,
        PayloadIdentity::Named { name } => Some(name.clone()),
    }
}

#[cfg(test)]
#[path = "root_inspect/tests.rs"]
mod tests;
