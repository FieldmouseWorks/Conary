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
//! from an empty materialization stand-in whose mode, ownership, and timestamps
//! belong to the inspecting process. That one node is reported with
//! [`RootInspectMetadata::Synthesized`] and with its mode, ownership, mtime,
//! xattrs, and content authority withheld; every node read from a committed
//! manifest reports [`RootInspectMetadata::Recorded`].
//!
//! Boot recovery can point `/current` at a valid generation with no state or
//! publication row. A stable link across the read is accepted as
//! [`RootInspectSource::CurrentGeneration`] with
//! [`RootInspectData::recovered_without_state`] set, so callers can tell that
//! the unknown IDs were never recorded rather than simply omitted. A try
//! session that owns the same generation decides first: an active or orphaned
//! session is an uncommitted trial and a rolled-back session a discarded one,
//! so both refuse; a kept session is the recorded promotion decision.

use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use conary_agent_contract::{InspectResult, OperationEnvelope, OperationStatus, RiskLevel};
use conary_core::generation::root_manifest::{CapturedSelectedRoot, GenerationRootEntry};
use conary_core::payload::{
    PayloadContentAuthority, PayloadIdentity, PayloadNode, PayloadNodeKind, ResolvedPayloadNode,
};
use conary_core::runtime_root::ConaryRuntimeRoot;
use serde::{Deserialize, Serialize};

use crate::commands::generation::selected_root::{
    SelectedRootBaseline, SelectedRootSource, read_selected_root_baseline,
};

pub(crate) const ROOT_INSPECT_SCHEMA_VERSION: u32 = 5;

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

/// One recorded extended attribute, in name order.
///
/// An xattr value is arbitrary bytes, so it is carried as standard base64
/// rather than as text. The recorded value is preserved exactly; the byte
/// length the human frame reports is recovered by decoding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct RootInspectXattr {
    pub(crate) name: String,
    pub(crate) value_base64: String,
}

impl RootInspectXattr {
    /// Exact recorded value size in bytes.
    ///
    /// The value is standard base64 of the recorded bytes, so a value this
    /// type encoded always decodes; a value from elsewhere that does not is
    /// reported as zero rather than panicking the renderer.
    pub(crate) fn value_len(&self) -> usize {
        BASE64
            .decode(self.value_base64.as_bytes())
            .map_or(0, |value| value.len())
    }
}

/// One recorded modification time.
///
/// Mirrors [`conary_core::payload::PayloadTimestamp`] field-for-field so the
/// sub-second component materialization restores is reported without loss.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct RootInspectTimestamp {
    pub(crate) seconds: i64,
    pub(crate) nanoseconds: u32,
}

/// Versioned `data` payload for `system.root.inspect`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct RootInspectData {
    pub(crate) schema_version: u32,
    pub(crate) snapshot_id: Option<i64>,
    pub(crate) changeset_id: Option<i64>,
    /// True when the baseline came from a stable `/current` generation the
    /// pinned snapshot never recorded and no uncommitted or rolled-back try
    /// session claims, so the
    /// IDs are unknown rather than merely absent. See
    /// `read_selected_root_baseline`.
    pub(crate) recovered_without_state: bool,
    pub(crate) source: RootInspectSource,
    pub(crate) path: String,
    pub(crate) present: bool,
    pub(crate) manifest: Option<RootManifestKind>,
    pub(crate) metadata: RootInspectMetadata,
    pub(crate) kind: Option<RootNodeKind>,
    pub(crate) mode: Option<u32>,
    pub(crate) mtime: Option<RootInspectTimestamp>,
    pub(crate) uid: Option<u64>,
    pub(crate) gid: Option<u64>,
    /// Source identity as recorded, a name or a numeric ID, alongside the
    /// resolved `uid` and `gid`.
    pub(crate) user: Option<String>,
    pub(crate) group: Option<String>,
    pub(crate) sha256: Option<String>,
    pub(crate) size: Option<u64>,
    pub(crate) symlink_target: Option<String>,
    pub(crate) hardlink_target: Option<String>,
    /// A regular node's primary identity or a hardlink entry's own identity.
    pub(crate) hardlink_identity: Option<String>,
    pub(crate) device_major: Option<u64>,
    pub(crate) device_minor: Option<u64>,
    /// Name-sorted recorded extended attributes; `Some([])` for a recorded node
    /// with none and `None` when `present` is false or the metadata is
    /// synthesized.
    pub(crate) xattrs: Option<Vec<RootInspectXattr>>,
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
    // withholds it through `apply_synthesized_root`. A database with no
    // installed trove has no committed root and no capture at all.
    let (reported, captured) = match read_selected_root_baseline(conn, runtime_root)? {
        SelectedRootBaseline::Captured { source, captured } => {
            (report_source(source), Some(captured))
        }
        SelectedRootBaseline::NoCommittedRoot => (
            ReportedSource {
                source: RootInspectSource::NoCommittedRoot,
                snapshot_id: None,
                changeset_id: None,
                recovered_without_state: false,
            },
            None,
        ),
    };

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
        mtime: None,
        uid: None,
        gid: None,
        user: None,
        group: None,
        sha256: None,
        size: None,
        symlink_target: None,
        hardlink_target: None,
        hardlink_identity: None,
        device_major: None,
        device_minor: None,
        xattrs: None,
    };

    // With no committed root there is no capture, so nothing is present, not
    // even a synthesized `/` node.
    if let Some(captured) = captured
        && let Some((manifest, recorded)) = find_captured_node(&captured, &normalized)
    {
        if data.source == RootInspectSource::DatabaseProjection && normalized == "/" {
            apply_synthesized_root(&mut data);
        } else {
            apply_node(&mut data, manifest, recorded);
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

/// One committed node selected for projection.
enum RecordedNode<'a> {
    /// A manifest entry, carrying its recorded path and optional content.
    Entry(&'a GenerationRootEntry),
    /// The generation root node, which records no entry path or content.
    Root(&'a ResolvedPayloadNode),
}

fn find_captured_node<'a>(
    captured: &'a CapturedSelectedRoot,
    path: &str,
) -> Option<(RootManifestKind, RecordedNode<'a>)> {
    if path == "/" {
        return Some((
            RootManifestKind::Root,
            RecordedNode::Root(&captured.generation.root),
        ));
    }
    let generation = captured
        .generation
        .entries
        .iter()
        .find(|entry| entry.path == path);
    if let Some(entry) = generation {
        return Some((RootManifestKind::Root, RecordedNode::Entry(entry)));
    }
    let state = captured
        .state
        .entries
        .iter()
        .find(|entry| entry.path == path);
    state.map(|entry| (RootManifestKind::MutableState, RecordedNode::Entry(entry)))
}

fn apply_node(data: &mut RootInspectData, manifest: RootManifestKind, recorded: RecordedNode<'_>) {
    data.present = true;
    data.manifest = Some(manifest);

    let (node, content) = match recorded {
        RecordedNode::Entry(entry) => {
            // The recorded path is already the normalized lookup path reported
            // as `data.path`, so it is not reported a second time.
            let GenerationRootEntry {
                path: _,
                node,
                content,
            } = entry;
            (node, content.as_ref())
        }
        // The generation root node records no entry path and no content.
        RecordedNode::Root(node) => (node, None),
    };

    let ResolvedPayloadNode { source, uid, gid } = node;
    data.uid = Some(*uid);
    data.gid = Some(*gid);

    let PayloadNode {
        kind,
        mode,
        user,
        group,
        mtime,
        xattrs,
    } = source;
    data.mode = Some(*mode & 0o7777);
    data.mtime = Some(RootInspectTimestamp {
        seconds: mtime.seconds,
        nanoseconds: mtime.nanoseconds,
    });
    data.user = source_identity(user);
    data.group = source_identity(group);
    // `PayloadNode::xattrs` is a `BTreeMap`, so this list is name-sorted.
    data.xattrs = Some(
        xattrs
            .iter()
            .map(|(name, value)| RootInspectXattr {
                name: name.clone(),
                value_base64: BASE64.encode(value),
            })
            .collect(),
    );

    match kind {
        PayloadNodeKind::Regular { hardlink_identity } => {
            data.kind = Some(RootNodeKind::Regular);
            data.hardlink_identity = hardlink_identity.clone();
        }
        PayloadNodeKind::Directory => data.kind = Some(RootNodeKind::Directory),
        PayloadNodeKind::Symlink { target } => {
            data.kind = Some(RootNodeKind::Symlink);
            data.symlink_target = Some(target.clone());
        }
        PayloadNodeKind::Hardlink { target, identity } => {
            data.kind = Some(RootNodeKind::Hardlink);
            data.hardlink_target = Some(target.clone());
            data.hardlink_identity = Some(identity.clone());
        }
        PayloadNodeKind::BlockDevice { major, minor } => {
            data.kind = Some(RootNodeKind::BlockDevice);
            data.device_major = Some(*major);
            data.device_minor = Some(*minor);
        }
        PayloadNodeKind::CharacterDevice { major, minor } => {
            data.kind = Some(RootNodeKind::CharacterDevice);
            data.device_major = Some(*major);
            data.device_minor = Some(*minor);
        }
        PayloadNodeKind::Fifo => data.kind = Some(RootNodeKind::Fifo),
        PayloadNodeKind::Socket => data.kind = Some(RootNodeKind::Socket),
    }

    let (sha256, size) = match content {
        // Regular files are the only nodes that record content authority.
        Some(content) => {
            let PayloadContentAuthority { sha256, size } = content;
            (Some(sha256.clone()), Some(*size))
        }
        None => (None, None),
    };
    data.sha256 = sha256;
    data.size = size;
}

/// Report the projection's stand-in `/` node without its ambient metadata.
///
/// The database projection has no committed manifest root; it synthesizes one
/// from the empty materialization destination, whose mode, ownership, and
/// timestamps belong to the inspecting process. The node is present as a
/// directory, but mode, ownership, mtime, xattrs, and content authority are
/// withheld and the record is marked synthesized so no caller mistakes them for
/// the committed root.
fn apply_synthesized_root(data: &mut RootInspectData) {
    data.present = true;
    data.metadata = RootInspectMetadata::Synthesized;
    data.manifest = Some(RootManifestKind::Root);
    data.kind = Some(RootNodeKind::Directory);
}

/// Why a lookup path cannot name one committed selected-root node.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum LookupPathError {
    #[error("selected-root inspect path must not be empty")]
    Empty,
    #[error(
        "selected-root inspect path {path:?} is relative; pass an absolute path starting with '/'"
    )]
    Relative { path: String },
    #[error("selected-root inspect path {path:?} contains '..'; inspect the exact committed path")]
    ParentComponent { path: String },
}

/// Normalize one absolute lookup path lexically without resolving the
/// filesystem.
fn normalize_lookup_path(path: &str) -> std::result::Result<String, LookupPathError> {
    if path.is_empty() {
        return Err(LookupPathError::Empty);
    }
    if !path.starts_with('/') {
        return Err(LookupPathError::Relative {
            path: path.to_string(),
        });
    }
    let mut components = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                return Err(LookupPathError::ParentComponent {
                    path: path.to_string(),
                });
            }
            other => components.push(other),
        }
    }
    Ok(format!("/{}", components.join("/")))
}

/// Report one source identity as recorded: its name, or its numeric ID.
///
/// The resolved `uid`/`gid` are reported separately, so a numeric source is
/// never silently folded into "no source identity" and a named source keeps
/// both its name and its resolved numeric ID.
fn source_identity(identity: &PayloadIdentity) -> Option<String> {
    match identity {
        PayloadIdentity::Numeric { id } => Some(id.to_string()),
        PayloadIdentity::Named { name } => Some(name.clone()),
    }
}

#[cfg(test)]
#[path = "root_inspect/tests.rs"]
mod tests;
