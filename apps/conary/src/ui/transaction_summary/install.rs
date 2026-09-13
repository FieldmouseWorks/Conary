// apps/conary/src/ui/transaction_summary/install.rs
//! Shared install/update preview and committed-result presentation.

use super::*;
use crate::commands::install_report::{InstallChange, InstallReport};

/// One observed change as owned row values, before the shared renderer borrows
/// them for a single table.
struct Row {
    change: Change,
    name: String,
    version: String,
    release: Option<String>,
    architecture: Option<String>,
    source_format: Option<SourcePackageFormat>,
    reason: Option<&'static str>,
}

impl Row {
    /// Version, release, and architecture keep their transition form for
    /// updates. The source format cell reports the incoming observation alone:
    /// the ecosystem the incoming package was observed in, never a reading of
    /// the replaced package, its version grammar, or its name.
    fn observed(change: &InstallChange) -> Self {
        let (group, observed, before) = match change {
            InstallChange::Install(observed) => (Change::Install, observed, None),
            InstallChange::Update { before, after } => (Change::Update, after, Some(before)),
            InstallChange::Remove(observed, _) => (Change::Remove, observed, None),
            InstallChange::Deconfigure(observed) => (Change::Deconfigure, observed, None),
        };
        let transition = |old: Option<&str>, new: Option<&str>| {
            format!("{} -> {}", old.unwrap_or("-"), new.unwrap_or("-"))
        };
        let identity = &observed.identity;
        let version = before.map_or_else(
            || identity.version.clone(),
            |old| transition(Some(&old.identity.version), Some(&identity.version)),
        );
        let release = before.map_or_else(
            || identity.release.clone(),
            |old| {
                Some(transition(
                    old.identity.release.as_deref(),
                    identity.release.as_deref(),
                ))
            },
        );
        let architecture = before.map_or_else(
            || identity.architecture.clone(),
            |old| {
                if old.identity.architecture == identity.architecture {
                    identity.architecture.clone()
                } else {
                    Some(transition(
                        old.identity.architecture.as_deref(),
                        identity.architecture.as_deref(),
                    ))
                }
            },
        );
        Self {
            change: group,
            name: identity.name.clone(),
            version,
            release,
            architecture,
            source_format: observed.source_format,
            reason: match change {
                InstallChange::Remove(_, kind) => Some(kind.as_str()),
                _ => None,
            },
        }
    }
}

fn install_lines(changes: &[InstallChange], preview: bool) -> Vec<String> {
    let rows: Vec<Row> = changes.iter().map(Row::observed).collect();
    let rows: Vec<_> = rows
        .iter()
        .map(|row| PackageChange {
            change: row.change,
            name: &row.name,
            version: &row.version,
            release: row.release.as_deref(),
            architecture: row.architecture.as_deref(),
            source_format: row.source_format,
            reason: row.reason,
        })
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
    use crate::commands::install_report::{ObservedPackage, PackageIdentity};
    use conary_core::repository::versioning::VersionScheme;

    #[test]
    fn shared_preview_retains_version_release_and_architecture_transitions() {
        let before = ObservedPackage {
            identity: PackageIdentity {
                name: "demo".into(),
                version: "1.0.0".into(),
                version_scheme: VersionScheme::Conary,
                release: Some("7".into()),
                architecture: Some("aarch64".into()),
            },
            source_format: Some(SourcePackageFormat::Rpm),
        };
        let after = ObservedPackage {
            identity: PackageIdentity {
                version: "2.0.0".into(),
                release: Some("8".into()),
                architecture: Some("x86_64".into()),
                ..before.identity.clone()
            },
            source_format: Some(SourcePackageFormat::Ccs),
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
            "Source format",
        ] {
            assert!(text.contains(expected), "{text}");
        }
        // An update reports the incoming observation, not a format transition.
        assert!(text.contains("ccs"), "{text}");
        assert!(!text.contains("rpm -> ccs"), "{text}");
        assert!(!text.contains("Applied"));
        assert!(!text.contains("Generation"));
    }

    #[test]
    fn every_change_kind_reports_only_its_observed_source_format() {
        let observed = |name: &str, version_scheme, source_format| ObservedPackage {
            identity: PackageIdentity {
                name: name.into(),
                version_scheme,
                version: "1.0.0".into(),
                release: None,
                architecture: Some("x86_64".into()),
            },
            source_format,
        };
        let rows = [
            InstallChange::Install(observed(
                "installed",
                VersionScheme::Conary,
                Some(SourcePackageFormat::Debian),
            )),
            InstallChange::Remove(
                observed(
                    "removed",
                    VersionScheme::Conary,
                    Some(SourcePackageFormat::Eopkg),
                ),
                conary_core::repository::dependency_model::RepositoryRequirementKind::Obsolete,
            ),
            InstallChange::Deconfigure(observed("unobserved", VersionScheme::Conary, None)),
            // Neither observation may be read off the version grammar: an
            // RPM-shaped version observed as a CCS source stays ccs, and a
            // Conary grammar observed as an RPM source stays rpm.
            InstallChange::Deconfigure(observed(
                "grammar-mismatch",
                VersionScheme::Rpm,
                Some(SourcePackageFormat::Ccs),
            )),
            InstallChange::Deconfigure(observed(
                "grammar-opposite",
                VersionScheme::Conary,
                Some(SourcePackageFormat::Rpm),
            )),
        ];
        let lines = install_lines(&rows, false).join("\n");
        let text = console::strip_ansi_codes(&lines);
        // Cells after the name: version, CCS release, architecture, source
        // format, then the reason column that only removals carry.
        let row = |name: &str| {
            text.lines()
                .find(|line| line.trim_start().starts_with(&format!("{name} ")))
                .unwrap_or_else(|| panic!("no row for {name} in {text}"))
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        assert_eq!(row("installed")[4], "deb");
        assert_eq!(row("removed")[4], "eopkg");
        // The removal keeps its relation reason beside the format cell.
        assert_eq!(row("removed")[5], "obsolete");
        assert_eq!(row("unobserved")[4], "-");
        assert_eq!(row("grammar-mismatch")[4], "ccs");
        assert_eq!(row("grammar-opposite")[4], "rpm");
    }
}
