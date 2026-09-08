// apps/conary/src/ui/transaction_summary/install.rs
//! Shared install/update preview and committed-result presentation.

use super::*;
use crate::commands::install_report::{InstallChange, InstallReport};

fn install_lines(changes: &[InstallChange], preview: bool) -> Vec<String> {
    let rows: Vec<_> = changes
        .iter()
        .map(|change| {
            let (kind, identity, before) = match change {
                InstallChange::Install(identity) => (Change::Install, identity, None),
                InstallChange::Update { before, after } => (Change::Update, after, Some(before)),
                InstallChange::Remove(identity) => (Change::Remove, identity, None),
                InstallChange::Deconfigure(identity) => (Change::Deconfigure, identity, None),
            };
            let transition = |old: Option<&str>, new: Option<&str>| {
                format!("{} -> {}", old.unwrap_or("-"), new.unwrap_or("-"))
            };
            let version = before.map_or_else(
                || identity.version.clone(),
                |old| transition(Some(&old.version), Some(&identity.version)),
            );
            let release = before.map_or_else(
                || identity.release.clone(),
                |old| {
                    Some(transition(
                        old.release.as_deref(),
                        identity.release.as_deref(),
                    ))
                },
            );
            let architecture = before.map_or_else(
                || identity.architecture.clone(),
                |old| {
                    if old.architecture == identity.architecture {
                        identity.architecture.clone()
                    } else {
                        Some(transition(
                            old.architecture.as_deref(),
                            identity.architecture.as_deref(),
                        ))
                    }
                },
            );
            (kind, &identity.name, version, release, architecture)
        })
        .collect();
    let rows: Vec<_> = rows
        .iter()
        .map(
            |(change, name, version, release, architecture)| PackageChange {
                change: *change,
                name,
                version,
                release: release.as_deref(),
                architecture: architecture.as_deref(),
            },
        )
        .collect();
    change_lines_with_heading(&rows, preview)
}

pub(crate) fn install_preview(changes: &[InstallChange]) {
    super::super::message(&install_lines(changes, true).join("\n"));
}

pub(crate) fn install_summary(report: &InstallReport, db_path: &str, dry_run: bool) {
    if dry_run {
        let mut lines = install_lines(&report.planned, true);
        lines.push(super::super::note_line(
            "Dry run: no package changes were applied.",
        ));
        super::super::message(&lines.join("\n"));
        return;
    }
    if report.commits.is_empty() {
        return;
    }
    let changes: Vec<_> = report
        .commits
        .iter()
        .flat_map(|commit| commit.changes.iter().cloned())
        .collect();
    let mut lines = install_lines(&changes, false);
    lines.push(super::super::field_line(
        "Installed file records",
        &report
            .commits
            .iter()
            .map(|commit| commit.file_records)
            .sum::<usize>()
            .to_string(),
    ));
    for commit in &report.commits {
        match &commit.publication {
            Some(publication) => lines.extend(
                closing_lines(commit.changeset_id, publication, db_path)
                    .into_iter()
                    .take(2),
            ),
            None => {
                lines.push(super::super::field_line(
                    "Changeset",
                    &commit.changeset_id.to_string(),
                ));
                lines.push(super::super::field_line(
                    "Generation",
                    "publication belongs to enclosing operation",
                ));
            }
        }
    }
    lines.push(super::super::note_line(&format!(
        "Inspect history: {}",
        database_command("conary system history", db_path)
    )));
    super::super::message(&lines.join("\n"));
    for commit in &report.commits {
        if let Some(publication) = &commit.publication {
            crate::commands::generation::publication::warn_if_publication_pending(
                commit.changeset_id,
                publication,
            );
        }
    }
}
