// apps/conary/src/ui/transaction_summary.rs
//! Applied package facts. Rendering never establishes mutation or publication success.

use crate::commands::generation::publication::PublicationOutcome;
use crate::commands::{LiveRootStats, TroveSnapshot};
use conary_core::db::models::Trove;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Change {
    Remove,
    Restore,
}

struct PackageChange<'a> {
    change: Change,
    name: &'a str,
    version: &'a str,
    release: Option<&'a str>,
    architecture: Option<&'a str>,
}

impl<'a> PackageChange<'a> {
    fn removed(trove: &'a Trove) -> Self {
        Self {
            change: Change::Remove,
            name: &trove.name,
            version: &trove.version,
            release: trove.package_release.as_deref(),
            architecture: trove.architecture.as_deref(),
        }
    }

    fn restored(snapshot: &'a TroveSnapshot) -> Self {
        Self {
            change: Change::Restore,
            name: &snapshot.name,
            version: &snapshot.version,
            release: snapshot.package_release.as_deref(),
            architecture: snapshot.architecture.as_deref(),
        }
    }
}

fn visible(value: &str) -> String {
    value
        .chars()
        .flat_map(|character| {
            if character.is_control() {
                character.escape_debug().collect::<Vec<_>>()
            } else {
                vec![character]
            }
        })
        .collect()
}

/// A copyable POSIX-shell command scoped to the same database as the operation.
/// Control-containing paths use an explicit placeholder rather than altered bytes.
pub(crate) fn database_command(command: &str, db_path: &str) -> String {
    if std::path::Path::new(db_path)
        == conary_core::runtime_root::ConaryRuntimeRoot::default().db_path()
    {
        return command.to_owned();
    }
    if db_path.chars().any(char::is_control) {
        return format!("{command} --db-path <PATH> (use the same database path)");
    }
    format!("{command} --db-path '{}'", db_path.replace('\'', "'\"'\"'"))
}

fn change_lines(changes: &[PackageChange<'_>]) -> Vec<String> {
    let mut lines = vec![super::heading_line("Applied package changes:")];
    for (change, label) in [(Change::Remove, "Removed"), (Change::Restore, "Restored")] {
        let rows: Vec<[String; 4]> = changes
            .iter()
            .filter(|entry| entry.change == change)
            .map(|entry| {
                [
                    visible(entry.name),
                    visible(entry.version),
                    entry.release.map(visible).unwrap_or_else(|| "-".into()),
                    entry
                        .architecture
                        .map(visible)
                        .unwrap_or_else(|| "-".into()),
                ]
            })
            .collect();
        if rows.is_empty() {
            continue;
        }
        lines.push(super::heading_line(&format!("  {label} ({}):", rows.len())));
        let headings = ["Package", "Version", "CCS release", "Architecture"].map(String::from);
        let widths: [usize; 4] = std::array::from_fn(|column| {
            rows.iter()
                .chain(std::iter::once(&headings))
                .map(|row| console::measure_text_width(&row[column]))
                .max()
                .unwrap_or(0)
        });
        for row in std::iter::once(&headings).chain(&rows) {
            let cells: Vec<String> = row
                .iter()
                .enumerate()
                .map(|(column, cell)| {
                    if column == 3 {
                        cell.clone()
                    } else {
                        format!(
                            "{cell}{}",
                            " ".repeat(widths[column] - console::measure_text_width(cell))
                        )
                    }
                })
                .collect();
            lines.push(format!("    {}", cells.join("  ")));
        }
    }
    if changes.is_empty() {
        lines.push("  No package identities changed.".into());
    }
    lines
}

fn closing_lines(
    changeset_id: i64,
    publication: &PublicationOutcome,
    db_path: &str,
) -> Vec<String> {
    let generation = if publication.needs_publication {
        "publication pending".to_owned()
    } else if let Some(number) = publication.generation_number {
        format!("{number} published")
    } else {
        "not reported".to_owned()
    };
    vec![
        super::field_line("Changeset", &changeset_id.to_string()),
        super::field_line("Generation", &generation),
        super::note_line(&format!(
            "Inspect history: {}",
            database_command("conary system history", db_path)
        )),
    ]
}

pub(crate) fn removal_summary(
    trove: &Trove,
    stats: &LiveRootStats,
    changeset_id: i64,
    publication: &PublicationOutcome,
    db_path: &str,
) {
    let mut lines = change_lines(&[PackageChange::removed(trove)]);
    lines.push(super::field_line(
        "Files removed",
        &stats.files_removed.to_string(),
    ));
    lines.push(super::field_line(
        "Directories removed",
        &stats.dirs_removed.to_string(),
    ));
    lines.extend(closing_lines(changeset_id, publication, db_path));
    let route = database_command(
        &format!("conary system state rollback {changeset_id} --yes"),
        db_path,
    );
    let when = if publication.needs_publication {
        "After publication, request rollback"
    } else {
        "Request rollback"
    };
    lines.push(super::note_line(&format!("{when}: {route}")));
    super::message(&lines.join("\n"));
}

pub(crate) fn rollback_summary(
    reversed_changeset_id: i64,
    rollback_changeset_id: i64,
    removed: &[Trove],
    restored: &[TroveSnapshot],
    publication: &PublicationOutcome,
    db_path: &str,
) {
    let changes: Vec<_> = removed
        .iter()
        .map(PackageChange::removed)
        .chain(restored.iter().map(PackageChange::restored))
        .collect();
    let mut lines = change_lines(&changes);
    lines.push(super::field_line(
        "Reversed changeset",
        &reversed_changeset_id.to_string(),
    ));
    lines.push(super::field_line(
        "Restored file records",
        &restored
            .iter()
            .map(|snapshot| snapshot.files.len())
            .sum::<usize>()
            .to_string(),
    ));
    lines.extend(closing_lines(rollback_changeset_id, publication, db_path));
    super::message(&lines.join("\n"));
}

#[cfg(test)]
mod tests;
