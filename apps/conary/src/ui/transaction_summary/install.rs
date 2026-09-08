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
                InstallChange::Remove(identity, _) => (Change::Remove, identity, None),
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
            let reason = match change {
                InstallChange::Remove(_, kind) => Some(kind.as_str()),
                _ => None,
            };
            (kind, &identity.name, version, release, architecture, reason)
        })
        .collect();
    let rows: Vec<_> = rows
        .iter()
        .map(
            |(change, name, version, release, architecture, reason)| PackageChange {
                change: *change,
                name,
                version,
                release: release.as_deref(),
                architecture: architecture.as_deref(),
                reason: *reason,
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
    let latest = report
        .commits
        .last()
        .expect("nonempty commits checked above");
    if report.commits.len() > 1 {
        lines.push(super::super::field_line(
            "Changesets",
            &report
                .commits
                .iter()
                .map(|commit| commit.changeset_id.to_string())
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }
    match &latest.publication {
        Some(publication) => lines.extend(
            closing_lines(latest.changeset_id, publication, db_path)
                .into_iter()
                .take(2),
        ),
        None => {
            lines.push(super::super::field_line(
                "Changeset",
                &latest.changeset_id.to_string(),
            ));
            lines.push(super::super::field_line(
                "Generation",
                "publication belongs to enclosing operation",
            ));
        }
    }
    lines.push(super::super::note_line(&format!(
        "Inspect history: {}",
        database_command("conary system history", db_path)
    )));
    super::super::message(&lines.join("\n"));
    if let Some(publication) = &latest.publication {
        crate::commands::generation::publication::warn_if_publication_pending(
            latest.changeset_id,
            publication,
        );
    }
}

/// Only an enclosing command may offer the latest mutation's rollback route.
pub(crate) fn install_rollback_route(report: &InstallReport, db_path: &str) {
    let Some(commit) = report.commits.last() else {
        return;
    };
    let Some(publication) = &commit.publication else {
        return;
    };
    let when = if publication.needs_publication {
        "After publication, request rollback of latest changeset"
    } else {
        "Request rollback of latest changeset"
    };
    super::super::message(&super::super::note_line(&format!(
        "{when}: {}",
        database_command(
            &format!("conary system state rollback {} --yes", commit.changeset_id),
            db_path
        )
    )));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::install_report::PackageIdentity;
    use conary_core::repository::versioning::VersionScheme;

    #[test]
    fn shared_preview_retains_version_release_and_architecture_transitions() {
        let before = PackageIdentity {
            name: "demo".into(),
            version: "1.0.0".into(),
            version_scheme: VersionScheme::Conary,
            release: Some("7".into()),
            architecture: Some("aarch64".into()),
        };
        let after = PackageIdentity {
            version: "2.0.0".into(),
            release: Some("8".into()),
            architecture: Some("x86_64".into()),
            ..before.clone()
        };
        let rows = [
            InstallChange::Update {
                before: before.clone(),
                after,
            },
            InstallChange::Remove(
                before.clone(),
                conary_core::repository::dependency_model::RepositoryRequirementKind::Obsolete,
            ),
            InstallChange::Deconfigure(before),
        ];
        let lines = install_lines(&rows, true).join("\n");
        let text = console::strip_ansi_codes(&lines);
        for expected in [
            "Planned package changes:",
            "Update (1):",
            "1.0.0 -> 2.0.0",
            "7 -> 8",
            "aarch64 -> x86_64",
            "Remove (1):",
            "Reason",
            "obsolete",
            "Deconfigure (1):",
        ] {
            assert!(text.contains(expected), "{text}");
        }
        assert!(!text.contains("Applied"));
        assert!(!text.contains("Generation"));
    }
}
