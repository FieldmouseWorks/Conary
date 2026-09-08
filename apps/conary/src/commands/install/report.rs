// apps/conary/src/commands/install/report.rs
//! Command observations carried explicitly through nested install operations.
//! These values report planner and committed transaction facts; they authorize nothing.

use crate::commands::generation::publication::PublicationOutcome;
use anyhow::{Context, Result};
use conary_core::db::models::Trove;
use conary_core::packages::PackageFormat;
use conary_core::transaction::PackageRelationPlan;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PackageIdentity {
    pub name: String,
    pub version: String,
    pub release: Option<String>,
    pub architecture: Option<String>,
}

impl PackageIdentity {
    pub(crate) fn package(pkg: &dyn PackageFormat) -> Self {
        Self {
            name: pkg.name().into(),
            version: pkg.version().into(),
            release: pkg.package_release().map(str::to_owned),
            architecture: pkg.architecture().map(str::to_owned),
        }
    }

    pub(crate) fn repository(package: &conary_core::db::models::RepositoryPackage) -> Self {
        Self {
            name: package.name.clone(),
            version: package.version.clone(),
            release: (!package.package_release.is_empty()).then(|| package.package_release.clone()),
            architecture: package.architecture.clone(),
        }
    }

    pub(crate) fn trove(trove: &Trove) -> Self {
        Self {
            name: trove.name.clone(),
            version: trove.version.clone(),
            release: trove.package_release.clone(),
            architecture: trove.architecture.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum InstallChange {
    Install(PackageIdentity),
    Update {
        before: PackageIdentity,
        after: PackageIdentity,
    },
    Remove(PackageIdentity),
    Deconfigure(PackageIdentity),
}

impl InstallChange {
    pub(crate) fn incoming(after: PackageIdentity, before: Option<&Trove>) -> Self {
        match before {
            Some(before) => Self::Update {
                before: PackageIdentity::trove(before),
                after,
            },
            None => Self::Install(after),
        }
    }
}

pub(crate) fn relation_changes(
    conn: &rusqlite::Connection,
    plan: &PackageRelationPlan,
) -> Result<Vec<InstallChange>> {
    let identity = |id| -> Result<_> {
        Ok(PackageIdentity::trove(
            &Trove::find_by_id(conn, id)?.context("planned relation package disappeared")?,
        ))
    };
    let mut changes = Vec::new();
    for removal in &plan.removals {
        changes.push(InstallChange::Remove(identity(removal.trove_id)?));
    }
    for deconfiguration in &plan.deconfigurations {
        changes.push(InstallChange::Deconfigure(identity(
            deconfiguration.package.trove_id,
        )?));
    }
    Ok(changes)
}

#[derive(Debug, Clone)]
pub(crate) struct InstallCommit {
    pub changes: Vec<InstallChange>,
    pub changeset_id: i64,
    pub file_records: usize,
    /// None only when publication belongs to an enclosing selected-root operation.
    pub publication: Option<PublicationOutcome>,
}

#[derive(Default)]
pub(crate) struct InstallReport {
    pub planned: Vec<InstallChange>,
    pub commits: Vec<InstallCommit>,
}

impl InstallReport {
    pub(crate) fn applied_targets(
        &self,
        targets: &std::collections::BTreeSet<PackageIdentity>,
    ) -> usize {
        self.commits
            .iter()
            .flat_map(|commit| &commit.changes)
            .filter_map(|change| match change {
                InstallChange::Install(after) | InstallChange::Update { after, .. }
                    if targets.contains(after) =>
                {
                    Some(after)
                }
                _ => None,
            })
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    }

    pub(crate) fn extend(&mut self, other: Self) {
        self.planned.extend(other.planned);
        self.commits.extend(other.commits);
    }

    pub(crate) fn render(&self, db_path: &str, dry_run: bool) {
        crate::ui::transaction_summary::install_summary(self, db_path, dry_run);
    }
}

pub(super) fn batch_changes(
    conn: &rusqlite::Connection,
    packages: &[super::batch::PreparedPackage],
) -> Result<Vec<InstallChange>> {
    let mut changes = Vec::new();
    let mut removed = std::collections::BTreeSet::new();
    let mut deconfigured = std::collections::BTreeSet::new();
    for package in packages {
        let before = package
            .old_trove_id()?
            .map(|id| Trove::find_by_id(conn, id))
            .transpose()?
            .flatten();
        changes.push(InstallChange::incoming(
            PackageIdentity {
                name: package.name.clone(),
                version: package.version.clone(),
                release: package.package_release.clone(),
                architecture: package.architecture.clone(),
            },
            before.as_ref(),
        ));
        let plan = PackageRelationPlan {
            removals: package
                .relation_removals
                .iter()
                .filter(|removal| removed.insert(removal.trove_id))
                .cloned()
                .collect(),
            deconfigurations: package
                .relation_deconfigurations
                .iter()
                .filter(|entry| deconfigured.insert(entry.package.trove_id))
                .cloned()
                .collect(),
        };
        changes.extend(relation_changes(conn, &plan)?);
    }
    Ok(changes)
}
