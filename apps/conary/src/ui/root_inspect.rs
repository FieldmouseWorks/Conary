// apps/conary/src/ui/root_inspect.rs
//! Read-only committed selected-root node frame.

use super::transaction_summary::visible;
use super::{Status, field, heading, row};
use crate::commands::{RootInspectData, RootInspectSource, RootManifestKind, RootNodeKind};

pub(crate) fn render(data: &RootInspectData) {
    heading("Committed selected root:");
    field("Path", &visible(&data.path));
    field("Source", source_label(data.source));
    field("Snapshot", &optional_i64(data.snapshot_id));
    field("Changeset", &optional_i64(data.changeset_id));

    if !data.present {
        row(
            Status::Missing,
            &["path", "not present in the committed selected root"],
        );
        return;
    }

    row(Status::Ok, &["present", kind_label(data.kind)]);
    field("Manifest", manifest_label(data.manifest));
    field("Metadata", data.metadata_label());
    field("Mode", &optional_mode(data.mode));
    field("UID", &optional_u64(data.uid));
    field("GID", &optional_u64(data.gid));
    field("User", &optional_text(data.user.as_deref()));
    field("Group", &optional_text(data.group.as_deref()));
    field("SHA-256", &optional_text(data.sha256.as_deref()));
    field(
        "Symlink target",
        &optional_text(data.symlink_target.as_deref()),
    );
    field(
        "Hardlink target",
        &optional_text(data.hardlink_target.as_deref()),
    );
}

fn source_label(source: RootInspectSource) -> &'static str {
    source.as_str()
}

fn kind_label(kind: Option<RootNodeKind>) -> &'static str {
    kind.map_or("unknown", RootNodeKind::as_str)
}

fn manifest_label(manifest: Option<RootManifestKind>) -> &'static str {
    manifest.map_or("-", RootManifestKind::as_str)
}

fn optional_i64(value: Option<i64>) -> String {
    value.map_or_else(|| "-".to_string(), |value| value.to_string())
}

fn optional_u64(value: Option<u64>) -> String {
    value.map_or_else(|| "-".to_string(), |value| value.to_string())
}

fn optional_text(value: Option<&str>) -> String {
    value.map_or_else(|| "-".to_string(), visible)
}

fn optional_mode(value: Option<u32>) -> String {
    value.map_or_else(|| "-".to_string(), |mode| format!("{mode:04o}"))
}
