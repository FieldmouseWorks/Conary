// apps/conary/src/ui/generation_pending.rs
//! Read-only publication debt facts; the command and core retain recovery authority.

use super::transaction_summary::visible;
use super::{Status, field, heading, message, note_line, row};
use crate::commands::generation::publication::PublicationOutcome;
use conary_core::db::models::{GenerationPublication, GenerationPublicationStatus};

pub(crate) fn render(database: &str, debts: &[GenerationPublication]) {
    heading("Pending generation publication debt:");
    field("Database", &visible(database));
    field("Records", &debts.len().to_string());
    if debts.is_empty() {
        message("No pending generation publication debt.");
        return;
    }

    for debt in debts {
        let status = match debt.status {
            GenerationPublicationStatus::Pending | GenerationPublicationStatus::Running => {
                Status::Pending
            }
            GenerationPublicationStatus::Failed => Status::Fail,
            GenerationPublicationStatus::Complete => Status::Ok,
            GenerationPublicationStatus::Abandoned => Status::Off,
        };
        row(status, &["Publication debt", &number(debt.id)]);
        field("Status", debt.status.as_str());
        field("Phase", debt.phase.as_str());
        field("Changeset", &number(debt.trigger_changeset_id));
        field("Generation", &number(debt.generation_number));
        field("State", &number(debt.state_number));
        field("Recorded database", &visible(&debt.db_path));
        field("Runtime root", &visible(&debt.runtime_root));
        field("Summary", &visible(&debt.summary));
        field("Retry count", &debt.retry_count.to_string());
        field(
            "Last error",
            &debt
                .last_error
                .as_deref()
                .map(visible)
                .unwrap_or_else(|| "-".into()),
        );
    }

    let retry = if database.chars().any(char::is_control) {
        "To retry publication, run conary system generation publish --yes with --db-path set to this same database path.".to_owned()
    } else {
        format!(
            "Retry pending publication: {}",
            PublicationOutcome::retry_command(database)
        )
    };
    message(&note_line(&retry));
}

fn number(value: Option<i64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".into())
}
