// apps/conary/src/commands/query/history.rs

//! Read changeset evidence and prepare command-scoped recovery guidance for UI rendering.

use super::super::open_db;
use crate::commands::generation::publication::PublicationOutcome;
use crate::ui::history::{self, FollowUpGuidance, HistoryFollowUp};
use anyhow::Result;
use conary_core::db::models::{Changeset, GenerationPublication, LifecycleEvent};

fn follow_up_guidance(
    follow_up: &crate::commands::DeferredFollowUp,
    db_path: &str,
) -> (Option<String>, FollowUpGuidance) {
    match crate::commands::classify_deferred_follow_up_kind(follow_up) {
        crate::commands::DeferredFollowUpKind::GenerationPublication => (
            follow_up
                .retry_command
                .as_ref()
                .map(|_| PublicationOutcome::retry_command(db_path)),
            FollowUpGuidance::None,
        ),
        crate::commands::DeferredFollowUpKind::GenerationPublicationNoBaseSystemInit => (
            None,
            FollowUpGuidance::AdoptOrInstallBaseSystem(
                conary_core::MissingBaseSystemPart::MissingInit,
            ),
        ),
        crate::commands::DeferredFollowUpKind::GenerationPublicationNoBaseSystemBootAssets => (
            None,
            FollowUpGuidance::AdoptOrInstallBaseSystem(
                conary_core::MissingBaseSystemPart::MissingBootAssets,
            ),
        ),
        crate::commands::DeferredFollowUpKind::Other => {
            (follow_up.retry_command.clone(), FollowUpGuidance::None)
        }
    }
}

fn history_follow_ups(changeset: &Changeset, db_path: &str) -> Result<Vec<HistoryFollowUp>> {
    Ok(
        crate::commands::deferred_follow_up(changeset.metadata.as_deref())?
            .into_iter()
            .map(|follow_up| {
                let (retry_command, guidance) = follow_up_guidance(&follow_up, db_path);
                HistoryFollowUp {
                    kind: follow_up.kind,
                    status: follow_up.status,
                    message: follow_up.message,
                    retry_command,
                    guidance,
                }
            })
            .collect(),
    )
}

/// Show changeset history without changing recorded state or publication authority.
pub fn cmd_history(db_path: &str) -> Result<()> {
    let conn = open_db(db_path)?;
    let changesets = Changeset::list_all(&conn)?;
    let publications = GenerationPublication::pending_recoverable(&conn)?;

    if changesets.is_empty() {
        history::empty();
        return Ok(());
    }
    history::heading();
    for changeset in &changesets {
        let deferred = history_follow_ups(changeset, db_path)?;
        let publication = changeset.id.and_then(|id| {
            publications
                .iter()
                .find(|publication| publication.trigger_changeset_id == Some(id))
        });
        let events = changeset
            .id
            .map(|id| LifecycleEvent::list_for_changeset(&conn, id))
            .transpose()?
            .unwrap_or_default();
        history::entry(changeset, publication, &deferred, &events);
    }
    history::finish(changesets.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{DeferredFollowUp, metadata_with_deferred_follow_up};

    #[test]
    fn publication_retry_uses_current_database_and_other_guidance_stays_recorded() {
        let mut changeset = Changeset::new("fixture".into());
        changeset.metadata = Some(
            metadata_with_deferred_follow_up(
                Vec::new(),
                vec![
                    DeferredFollowUp {
                        kind: "generation_publication".into(),
                        status: "failed".into(),
                        message: "retry publication".into(),
                        retry_command: Some("stale database command".into()),
                    },
                    DeferredFollowUp {
                        kind: "other".into(),
                        status: "pending".into(),
                        message: "retained guidance".into(),
                        retry_command: Some("recorded command".into()),
                    },
                    DeferredFollowUp {
                        kind: "other".into(),
                        status: "pending".into(),
                        message: "no recorded remedy".into(),
                        retry_command: None,
                    },
                ],
            )
            .unwrap(),
        );
        let records = history_follow_ups(&changeset, "/tmp/operator's history.db").unwrap();
        assert_eq!(records.len(), 3);
        assert_eq!(
            records[0].retry_command.as_deref(),
            Some(
                "conary system generation publish --yes --db-path='/tmp/operator'\"'\"'s history.db'"
            )
        );
        assert_eq!(
            records[1].retry_command.as_deref(),
            Some("recorded command")
        );
        assert_eq!(records[2].retry_command, None);
    }

    #[test]
    fn no_base_publication_guidance_records_and_classifies_each_missing_part() {
        use crate::commands::generation::publication::PublicationFailureKind;
        use crate::commands::publication_deferred_follow_up;
        use conary_core::MissingBaseSystemPart;

        for (missing, expected) in [
            (
                MissingBaseSystemPart::MissingInit,
                FollowUpGuidance::AdoptOrInstallBaseSystem(MissingBaseSystemPart::MissingInit),
            ),
            (
                MissingBaseSystemPart::MissingBootAssets,
                FollowUpGuidance::AdoptOrInstallBaseSystem(
                    MissingBaseSystemPart::MissingBootAssets,
                ),
            ),
        ] {
            let mut changeset = Changeset::new("fixture".into());
            changeset.metadata = Some(
                metadata_with_deferred_follow_up(
                    Vec::new(),
                    vec![publication_deferred_follow_up(
                        Some(PublicationFailureKind::NoBaseSystem(missing)),
                        "generation publication is pending".into(),
                        "/tmp/history.db",
                    )],
                )
                .unwrap(),
            );
            let records = history_follow_ups(&changeset, "/tmp/history.db").unwrap();
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].guidance, expected, "{missing:?}");
            assert_eq!(records[0].retry_command, None);
        }
    }

    #[test]
    fn missing_metadata_has_no_follow_up_and_obsolete_metadata_still_refuses() {
        let mut changeset = Changeset::new("fixture".into());
        assert!(
            history_follow_ups(&changeset, "/tmp/history.db")
                .unwrap()
                .is_empty()
        );
        changeset.metadata = Some(r#"{"schema":"conary.changeset.metadata.v5"}"#.into());
        let error = history_follow_ups(&changeset, "/tmp/history.db").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Unsupported changeset metadata schema")
        );
    }
}
