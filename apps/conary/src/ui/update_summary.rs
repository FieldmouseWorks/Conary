// apps/conary/src/ui/update_summary.rs
//! Collection update presentation, driven by explicit request outcomes.

mod selection;
pub(crate) use selection::collection_selection_summary;

use super::{Status, field_line, heading_line, row_line};
use crate::commands::update_outcome::{
    CollectionUpdateEntry, CollectionUpdateStatus, UpdateOutcome,
};

fn summary_lines(name: &str, preview: bool, entries: &[CollectionUpdateEntry]) -> Vec<String> {
    let mut planned = 0;
    let mut applied = 0;
    let mut unchanged = 0;
    let mut failed = 0;
    let mut rows = Vec::with_capacity(entries.len());
    for entry in entries {
        let (status, detail) = match entry.status {
            CollectionUpdateStatus::Completed(UpdateOutcome::NoChanges) => {
                unchanged += 1;
                (Status::Skip, "no changes".into())
            }
            CollectionUpdateStatus::Completed(UpdateOutcome::Planned { packages }) => {
                planned += packages;
                (Status::Pending, format!("{packages} planned"))
            }
            CollectionUpdateStatus::Completed(UpdateOutcome::Applied { packages }) => {
                applied += packages;
                (Status::Ok, format!("{packages} applied"))
            }
            CollectionUpdateStatus::Failed => {
                failed += 1;
                (Status::Fail, "request failed".into())
            }
        };
        rows.push(row_line(status, &[&entry.target, &detail]));
    }
    let mut lines = vec![
        heading_line(if preview {
            "Collection update preview"
        } else {
            "Collection update results"
        }),
        field_line("Collection", name),
    ];
    if preview || planned > 0 {
        lines.push(field_line("Planned packages", &planned.to_string()));
    }
    if !preview || applied > 0 {
        lines.push(field_line("Applied packages", &applied.to_string()));
    }
    lines.push(field_line("Unchanged requests", &unchanged.to_string()));
    lines.push(field_line("Failed requests", &failed.to_string()));
    lines.extend(rows);
    if preview && applied == 0 {
        lines.push("Dry run: no updates were applied.".into());
    } else if failed > 0 {
        lines.push(super::note_line(
            "Failed requests may have left applied changes. Inspect package state before retrying.",
        ));
    }
    lines
}

pub(crate) fn collection_update_summary(
    name: &str,
    preview: bool,
    entries: &[CollectionUpdateEntry],
) {
    super::message(&summary_lines(name, preview, entries).join("\n"));
}

#[cfg(test)]
mod tests;
