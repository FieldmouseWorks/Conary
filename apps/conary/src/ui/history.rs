// apps/conary/src/ui/history.rs
//! Changeset history frames built from typed command observations.
//!
//! The command boundary supplies every fact: typed `Changeset` rows, an
//! optional pending-recoverable `GenerationPublication`, the ordered
//! `LifecycleEvent` evidence, and already-classified follow-up records. This
//! module performs no database or filesystem lookup, no metadata
//! deserialization, no retry derivation, and no source or eligibility
//! classification. An absent observation renders no field and never implies
//! success, publication, or recovery.

use super::transaction_summary::visible;
use super::{Status, field_line, heading_line, message, note_line, row_line};
use conary_core::db::models::{Changeset, GenerationPublication, LifecycleEvent};

/// Typed remediation guidance selected by the command boundary from persisted
/// follow-up kind authority. Rendering never infers this from the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FollowUpGuidance {
    None,
    /// Publication is pending because the selected root has no base system.
    /// Re-running publication cannot succeed until `/sbin/init` exists.
    AdoptOrInstallBaseSystem,
}

/// One deferred follow-up already classified by the command boundary.
///
/// `retry_command` is prepared guidance from the owning classification and the
/// same database context. It is displayed data; this module never runs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HistoryFollowUp {
    pub kind: String,
    pub status: String,
    pub message: String,
    pub retry_command: Option<String>,
    pub guidance: FollowUpGuidance,
}

/// Open the history frame. The caller replaces it with [`empty`] when no
/// changeset observation exists.
pub(crate) fn heading() {
    message(&heading_line("Changeset history:"));
}

/// Exact empty-state text.
pub(crate) fn empty() {
    message("No changeset history.");
}

/// Close the frame with the retained changeset count.
pub(crate) fn finish(count: usize) {
    message(&field_line("Total changesets", &count.to_string()));
}

/// Render one changeset record. One message per record keeps active progress
/// rows from interleaving inside the record.
pub(crate) fn entry(
    changeset: &Changeset,
    publication: Option<&GenerationPublication>,
    deferred: &[HistoryFollowUp],
    events: &[LifecycleEvent],
) {
    message(&entry_lines(changeset, publication, deferred, events).join("\n"));
}

fn entry_lines(
    changeset: &Changeset,
    publication: Option<&GenerationPublication>,
    deferred: &[HistoryFollowUp],
    events: &[LifecycleEvent],
) -> Vec<String> {
    let id = changeset
        .id
        .map(|id| id.to_string())
        .unwrap_or_else(|| "-".to_owned());
    let mut lines = vec![
        heading_line(&format!("Changeset {id}:")),
        field_line("Description", &visible(&changeset.description)),
        field_line("Kind", changeset.kind.as_str()),
        field_line("Status", changeset.status.as_str()),
    ];
    for (label, timestamp) in [
        ("Created", changeset.created_at.as_ref()),
        ("Applied", changeset.applied_at.as_ref()),
        ("Rolled back", changeset.rolled_back_at.as_ref()),
    ] {
        if let Some(timestamp) = timestamp {
            lines.push(field_line(label, &visible(timestamp)));
        }
    }
    if let Some(reverts_changeset_id) = changeset.reverts_changeset_id {
        lines.push(field_line(
            "Reverses changeset",
            &reverts_changeset_id.to_string(),
        ));
    }
    if let Some(reversed_by_changeset_id) = changeset.reversed_by_changeset_id {
        lines.push(field_line(
            "Reversed by changeset",
            &reversed_by_changeset_id.to_string(),
        ));
    }
    if let Some(publication) = publication {
        lines.push(field_line(
            "Generation publication",
            publication.status.as_str(),
        ));
    }
    if !deferred.is_empty() {
        let deferred_count = deferred.len();
        lines.push(heading_line(&format!("Deferred work ({deferred_count}):")));
        for follow_up in deferred {
            lines.extend(follow_up_lines(follow_up));
        }
    }
    for event in events {
        lines.extend(lifecycle_event_lines(event));
    }
    lines
}

/// Every record is retained in input order, including exact duplicates.
fn follow_up_lines(follow_up: &HistoryFollowUp) -> Vec<String> {
    let mut lines = vec![
        field_line("Kind", &visible(&follow_up.kind)),
        field_line("Status", &visible(&follow_up.status)),
        field_line("Reason", &visible(&follow_up.message)),
    ];
    match follow_up.guidance {
        FollowUpGuidance::None => {
            if let Some(retry_command) = &follow_up.retry_command {
                lines.push(note_line(&format!("Retry: {}", visible(retry_command))));
            }
        }
        FollowUpGuidance::AdoptOrInstallBaseSystem => {
            for guidance in crate::ui::publication::NO_BASE_SYSTEM_GUIDANCE {
                lines.push(note_line(guidance));
            }
        }
    }
    lines
}

/// One shared warning row per continued lifecycle failure, followed by the
/// typed event facts exactly as recorded.
fn lifecycle_event_lines(event: &LifecycleEvent) -> Vec<String> {
    vec![
        row_line(Status::Warn, &["Continued lifecycle failure"]),
        field_line("Package", &visible(&event.source_package)),
        field_line("Version", &visible(&event.source_version)),
        field_line("Entry", &visible(&event.source_entry)),
        field_line("Failure", event.failure_kind.as_str()),
        field_line("Phase", &visible(&event.phase)),
        field_line("Requested sandbox", event.requested_sandbox_mode.as_str()),
        field_line("Effective sandbox", event.effective_sandbox.as_str()),
        field_line("Reason", &visible(&event.message)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use conary_core::db::models::{
        ChangesetKind, ChangesetStatus, GenerationPublicationPhase, GenerationPublicationStatus,
    };
    use conary_core::scriptlet::{EffectiveSandbox, SandboxMode, ScriptletFailureKind};

    fn plain() {
        console::set_colors_enabled(false);
    }

    fn changeset(id: Option<i64>) -> Changeset {
        let mut changeset = Changeset::new("fixture change".to_owned());
        changeset.id = id;
        changeset
    }

    fn publication(status: GenerationPublicationStatus) -> GenerationPublication {
        GenerationPublication {
            id: Some(1),
            trigger_changeset_id: Some(7),
            published_through_changeset_id: None,
            tx_uuid: None,
            selected_root_snapshot_id: None,
            db_path: "/tmp/fixture.db".to_owned(),
            runtime_root: "/tmp/fixture-root".to_owned(),
            phase: GenerationPublicationPhase::PendingBuild,
            status,
            state_number: None,
            generation_number: None,
            summary: "fixture".to_owned(),
            config_transaction: Default::default(),
            last_error: None,
            retry_count: 0,
            recoverable: true,
            created_at: None,
            updated_at: None,
            completed_at: None,
        }
    }

    fn lifecycle_event() -> LifecycleEvent {
        LifecycleEvent {
            id: 1,
            changeset_id: 7,
            sequence: 0,
            source_package: "fixture".to_owned(),
            source_version: "1.0.0".to_owned(),
            source_entry: "rpm:%post".to_owned(),
            failure_kind: ScriptletFailureKind::ScriptExited,
            requested_sandbox_mode: SandboxMode::Always,
            effective_sandbox: EffectiveSandbox::TargetRoot,
            phase: "post-install".to_owned(),
            message: "script returned 42".to_owned(),
            created_at: "2026-08-09 12:00:00".to_owned(),
        }
    }

    #[test]
    fn missing_observations_render_no_absent_facts() {
        plain();
        let lines = entry_lines(&changeset(None), None, &[], &[]);
        assert_eq!(lines[0], "Changeset -:");
        assert_eq!(lines[1], "  Description: fixture change");
        assert_eq!(lines[2], "  Kind: mutation");
        assert_eq!(lines[3], "  Status: pending");
        assert_eq!(lines.len(), 4);
        for absent in [
            "Created",
            "Applied",
            "Rolled back",
            "Reverses",
            "Reversed by",
            "Generation publication",
            "Deferred work",
            "Continued lifecycle failure",
        ] {
            assert!(
                !lines.iter().any(|line| line.contains(absent)),
                "unexpected {absent}"
            );
        }
    }

    #[test]
    fn typed_rollback_relationships_and_timestamps_render_exactly() {
        plain();
        let mut changeset = changeset(Some(7));
        changeset.kind = ChangesetKind::Rollback;
        changeset.status = ChangesetStatus::Applied;
        changeset.created_at = Some("2026-05-14 12:00:00".to_owned());
        changeset.applied_at = Some("2026-05-14 12:01:00".to_owned());
        changeset.reverts_changeset_id = Some(3);
        changeset.reversed_by_changeset_id = Some(9);
        assert_eq!(
            entry_lines(&changeset, None, &[], &[]),
            vec![
                "Changeset 7:".to_owned(),
                "  Description: fixture change".to_owned(),
                "  Kind: rollback".to_owned(),
                "  Status: applied".to_owned(),
                "  Created: 2026-05-14 12:00:00".to_owned(),
                "  Applied: 2026-05-14 12:01:00".to_owned(),
                "  Reverses changeset: 3".to_owned(),
                "  Reversed by changeset: 9".to_owned(),
            ]
        );
    }

    #[test]
    fn pending_publication_reports_recorded_status_only() {
        plain();
        let publication = publication(GenerationPublicationStatus::Failed);
        let lines = entry_lines(&changeset(Some(7)), Some(&publication), &[], &[]);
        assert_eq!(lines[4], "  Generation publication: failed");
        assert_eq!(lines.len(), 5);
    }

    #[test]
    fn control_characters_stay_escaped_inside_fields() {
        plain();
        let mut changeset = changeset(Some(8));
        changeset.description = "line one\nline two\u{1b}[31m".to_owned();
        let deferred = [HistoryFollowUp {
            kind: "generation\u{7}publication".to_owned(),
            status: "failed".to_owned(),
            message: "boom\nsplit".to_owned(),
            retry_command: Some("conary\u{1b}publish".to_owned()),
            guidance: FollowUpGuidance::None,
        }];
        let lines = entry_lines(&changeset, None, &deferred, &[]);
        assert_eq!(lines[1], "  Description: line one\\nline two\\u{1b}[31m");
        assert_eq!(lines[4], "Deferred work (1):");
        assert_eq!(lines[5], "  Kind: generation\\u{7}publication");
        assert_eq!(lines[6], "  Status: failed");
        assert_eq!(lines[7], "  Reason: boom\\nsplit");
        assert_eq!(lines[8], "note: Retry: conary\\u{1b}publish");
        assert_eq!(lines.len(), 9);
        assert!(
            lines
                .iter()
                .all(|line| !line.contains('\u{1b}') && !line.contains('\n')),
            "{lines:?}"
        );
    }

    #[test]
    fn deferred_records_render_in_order_with_one_retry_note_per_command() {
        plain();
        let deferred = [
            HistoryFollowUp {
                kind: "generation_publication".to_owned(),
                status: "failed".to_owned(),
                message: "root is not self-contained".to_owned(),
                retry_command: Some("conary system generation publish --yes".to_owned()),
                guidance: FollowUpGuidance::None,
            },
            HistoryFollowUp {
                kind: "generation_publication".to_owned(),
                status: "failed".to_owned(),
                message: "root is not self-contained".to_owned(),
                retry_command: None,
                guidance: FollowUpGuidance::None,
            },
        ];
        let lines = entry_lines(&changeset(Some(7)), None, &deferred, &[]);
        assert_eq!(lines[4], "Deferred work (2):");
        assert_eq!(lines[5], "  Kind: generation_publication");
        assert_eq!(lines[6], "  Status: failed");
        assert_eq!(lines[7], "  Reason: root is not self-contained");
        assert_eq!(
            lines[8],
            "note: Retry: conary system generation publish --yes"
        );
        assert_eq!(lines[9], "  Kind: generation_publication");
        assert_eq!(lines[10], "  Status: failed");
        assert_eq!(lines[11], "  Reason: root is not self-contained");
        assert_eq!(lines.len(), 12);
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.starts_with("note: Retry:"))
                .count(),
            1
        );
    }

    #[test]
    fn no_base_system_follow_up_renders_guidance_without_retry_note() {
        plain();
        let deferred = [HistoryFollowUp {
            kind: "generation_publication_no_base_system".to_owned(),
            status: "pending".to_owned(),
            message: crate::ui::publication::NO_BASE_SYSTEM_REASON.to_owned(),
            retry_command: None,
            guidance: FollowUpGuidance::AdoptOrInstallBaseSystem,
        }];
        let lines = entry_lines(&changeset(Some(7)), None, &deferred, &[]);
        assert_eq!(lines[4], "Deferred work (1):");
        assert_eq!(lines[5], "  Kind: generation_publication_no_base_system");
        assert_eq!(lines[6], "  Status: pending");
        let reason = field_line("Reason", crate::ui::publication::NO_BASE_SYSTEM_REASON);
        assert_eq!(lines[7], reason);
        assert_eq!(
            lines[8],
            "note: The package change is committed and will publish once a base system is present."
        );
        assert_eq!(
            lines[9],
            "note: Adopt this machine's native system: conary system adopt --system"
        );
        assert_eq!(
            lines[10],
            "note: Or install a base system that provides /sbin/init from a repository."
        );
        assert_eq!(lines.len(), 11);
    }

    #[test]
    fn lifecycle_failure_retains_typed_fields_in_order() {
        plain();
        let events = [lifecycle_event()];
        let lines = entry_lines(&changeset(Some(7)), None, &[], &events);
        assert_eq!(
            lines[4],
            row_line(Status::Warn, &["Continued lifecycle failure"])
        );
        assert_eq!(lines[5], "  Package: fixture");
        assert_eq!(lines[6], "  Version: 1.0.0");
        assert_eq!(lines[7], "  Entry: rpm:%post");
        assert_eq!(lines[8], "  Failure: ScriptExited");
        assert_eq!(lines[9], "  Phase: post-install");
        assert_eq!(lines[10], "  Requested sandbox: always");
        assert_eq!(lines[11], "  Effective sandbox: target-root");
        assert_eq!(lines[12], "  Reason: script returned 42");
        assert_eq!(lines.len(), 13);
    }
}
