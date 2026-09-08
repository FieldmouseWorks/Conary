// apps/conary/src/ui/update_summary/selection.rs
//! Explain collection selection without treating skipped members as current.

use crate::commands::update_outcome::{CollectionSelectionEntry, CollectionSelectionStatus};
use crate::ui::{Status, field_line, heading_line, note_line, row_line};

fn selection_lines(
    name: &str,
    members: usize,
    security: bool,
    entries: &[CollectionSelectionEntry],
) -> Vec<String> {
    let mut selected = 0;
    let mut pinned = 0;
    let mut external = 0;
    let mut missing = 0;
    let mut ineligible = 0;
    let mut rows = Vec::with_capacity(entries.len());
    for entry in entries {
        let (status, detail) = match &entry.status {
            CollectionSelectionStatus::Selected => {
                selected += 1;
                (Status::Pending, "selected for update".into())
            }
            CollectionSelectionStatus::Pinned => {
                pinned += 1;
                (Status::Skip, "pinned; not checked".into())
            }
            CollectionSelectionStatus::ExternallyManaged { guidance } => {
                external += 1;
                (Status::Skip, format!("external authority: {guidance}"))
            }
            CollectionSelectionStatus::NotInstalled => {
                missing += 1;
                (Status::Missing, "not installed".into())
            }
            CollectionSelectionStatus::NoEligibleUpdate => {
                ineligible += 1;
                (
                    Status::Info,
                    if security {
                        "no eligible security update"
                    } else {
                        "no eligible update"
                    }
                    .into(),
                )
            }
        };
        rows.push(row_line(status, &[&entry.target, &detail]));
    }
    let mut lines = vec![
        heading_line("Collection update selection"),
        field_line("Collection", name),
        field_line("Members", &members.to_string()),
        field_line("Selected packages", &selected.to_string()),
    ];
    for (label, count) in [
        ("Pinned packages", pinned),
        ("Externally managed packages", external),
        ("Packages without eligible updates", ineligible),
        ("Uninstalled members", missing),
    ] {
        if count > 0 {
            lines.push(field_line(label, &count.to_string()));
        }
    }
    lines.extend(rows);
    if members == 0 {
        lines.push("Collection has no members.".into());
    } else if selected == 0 {
        lines.push(
            if security {
                "No eligible security updates selected."
            } else {
                "No eligible updates selected."
            }
            .into(),
        );
    }
    if external > 0 {
        lines.push(note_line("Run 'conary system adopt --refresh' after native package-manager changes before retrying Conary workflows."));
    }
    if missing > 0 {
        lines.push(note_line(
            "Uninstalled members are not installed by an update.",
        ));
    }
    lines
}

pub(crate) fn collection_selection_summary(
    name: &str,
    members: usize,
    security: bool,
    entries: &[CollectionSelectionEntry],
) {
    crate::ui::message(&selection_lines(name, members, security, entries).join("\n"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_member_can_have_multiple_installed_variant_results() {
        console::set_colors_enabled(false);
        let entries = [
            CollectionSelectionEntry {
                target: "demo 1.0 [x86_64]".into(),
                status: CollectionSelectionStatus::Selected,
            },
            CollectionSelectionEntry {
                target: "demo 1.0 [aarch64]".into(),
                status: CollectionSelectionStatus::Pinned,
            },
        ];
        let text = selection_lines("base", 1, false, &entries).join("\n");
        assert!(text.contains("  Members: 1\n"), "{text}");
        assert!(text.contains("  Selected packages: 1\n"), "{text}");
        assert!(text.contains("  Pinned packages: 1\n"), "{text}");
        assert!(
            text.contains("demo 1.0 [x86_64]  selected for update"),
            "{text}"
        );
        assert!(
            text.contains("demo 1.0 [aarch64]  pinned; not checked"),
            "{text}"
        );
        assert!(!text.contains("No eligible updates selected."), "{text}");
    }
}
