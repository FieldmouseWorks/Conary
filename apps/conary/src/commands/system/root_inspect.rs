// apps/conary/src/commands/system/root_inspect.rs

//! Read-only inspection of the committed selected-root node at one path.
//!
//! The authority is the same typed selection the install path prepares: the
//! newest committed selected-root publication snapshot, then the current
//! generation artifact, then the installed database projection, and finally an
//! explicit no-committed-root state. This module looks up one exact path in the
//! selected manifests and never derives a second projection.

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

pub(crate) const ROOT_INSPECT_SCHEMA_VERSION: u32 = 1;

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

/// Versioned `data` payload for `system.root.inspect`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct RootInspectData {
    pub(crate) schema_version: u32,
    pub(crate) snapshot_id: Option<i64>,
    pub(crate) changeset_id: Option<i64>,
    pub(crate) source: RootInspectSource,
    pub(crate) path: String,
    pub(crate) present: bool,
    pub(crate) manifest: Option<RootManifestKind>,
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

/// Open the selected database and render the result as typed JSON or human text.
pub fn cmd_root_inspect(db_path: &str, path: &str, json: bool) -> Result<()> {
    let conn = crate::commands::open_db(db_path)?;
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
    // The private empty directory stands in for the empty materialization
    // destination a first-generation projection reads for root metadata and
    // package-unclaimed parent closure.
    let empty_root = tempfile::TempDir::new()
        .context("failed to create the private selected-root inspection directory")?;
    let (source, captured) =
        read_selected_root_baseline_with_source(conn, runtime_root, empty_root.path())?;
    let (source, snapshot_id, changeset_id) = report_source(source);

    let mut data = RootInspectData {
        schema_version: ROOT_INSPECT_SCHEMA_VERSION,
        snapshot_id,
        changeset_id,
        source,
        path: normalized.clone(),
        present: false,
        manifest: None,
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
        apply_node(&mut data, manifest, node, content);
    }

    Ok(data)
}

/// Map the typed selection source onto its serialized report fields.
fn report_source(source: SelectedRootSource) -> (RootInspectSource, Option<i64>, Option<i64>) {
    match source {
        SelectedRootSource::PendingSnapshot {
            snapshot_id,
            changeset_id,
        } => (
            RootInspectSource::PendingSnapshot,
            Some(snapshot_id),
            changeset_id,
        ),
        SelectedRootSource::CurrentGeneration {
            snapshot_id,
            changeset_id,
        } => (
            RootInspectSource::CurrentGeneration,
            snapshot_id,
            changeset_id,
        ),
        SelectedRootSource::DatabaseProjection { changeset_id } => {
            (RootInspectSource::DatabaseProjection, None, changeset_id)
        }
        SelectedRootSource::NoCommittedRoot => (RootInspectSource::NoCommittedRoot, None, None),
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
