// apps/conary/src/commands/install/report.rs
//! Command observations carried explicitly through nested install operations.
//! These values report planner and committed transaction facts; they authorize nothing.

use crate::commands::generation::publication::PublicationOutcome;
use anyhow::{Context, Result};
use conary_core::db::models::Trove;
use conary_core::packages::PackageFormat;
use conary_core::repository::dependency_model::SourcePackageFormat;
use conary_core::transaction::PackageRelationPlan;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PackageIdentity {
    pub name: String,
    pub version: String,
    pub version_scheme: conary_core::repository::versioning::VersionScheme,
    pub release: Option<String>,
    pub architecture: Option<String>,
}

impl PackageIdentity {
    pub(crate) fn package(pkg: &dyn PackageFormat) -> Self {
        Self {
            name: pkg.name().into(),
            version: pkg.version().into(),
            version_scheme: pkg.version_scheme(),
            release: pkg.package_release().map(str::to_owned),
            architecture: pkg.architecture().map(str::to_owned),
        }
    }

    pub(crate) fn repository(package: &conary_core::db::models::RepositoryPackage) -> Self {
        Self {
            name: package.name.clone(),
            version: package.version.clone(),
            version_scheme: package.version_scheme,
            release: (!package.package_release.is_empty()).then(|| package.package_release.clone()),
            architecture: package.architecture.clone(),
        }
    }

    pub(crate) fn trove(trove: &Trove) -> Self {
        Self {
            name: trove.name.clone(),
            version: trove.version.clone(),
            version_scheme: trove.version_scheme,
            release: trove.package_release.clone(),
            architecture: trove.architecture.clone(),
        }
    }
}

/// Presentation facts remain separate from identities used for selection and counts.
#[derive(Debug, Clone)]
pub(crate) struct ObservedPackage {
    pub identity: PackageIdentity,
    pub source_format: Option<SourcePackageFormat>,
}

impl ObservedPackage {
    pub(crate) fn package(pkg: &dyn PackageFormat, semantics: super::InstallSemantics) -> Self {
        Self::prepared(PackageIdentity::package(pkg), semantics)
    }

    fn prepared(identity: PackageIdentity, semantics: super::InstallSemantics) -> Self {
        use super::semantics::PreparedSourceKind;
        use conary_core::packages::PackageFormatType;

        // The prepared source was classified from the artifact and its lifecycle
        // contract. A CCS package's version grammar alone does not name its source.
        let source_format = match semantics.source {
            PreparedSourceKind::Ccs => SourcePackageFormat::Ccs,
            PreparedSourceKind::NativePackage { format } => match format {
                PackageFormatType::Rpm => SourcePackageFormat::Rpm,
                PackageFormatType::Deb => SourcePackageFormat::Debian,
                PackageFormatType::Arch => SourcePackageFormat::Alpm,
                PackageFormatType::Eopkg => SourcePackageFormat::Eopkg,
            },
        };
        Self {
            identity,
            source_format: Some(source_format),
        }
    }

    fn trove(trove: &Trove) -> Self {
        Self {
            identity: PackageIdentity::trove(trove),
            source_format: trove
                .native_package_identity
                .as_ref()
                .map(conary_core::packages::InstalledPackageIdentity::source_package_format),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum InstallChange {
    Install(ObservedPackage),
    Update {
        before: ObservedPackage,
        after: ObservedPackage,
    },
    Remove(
        ObservedPackage,
        conary_core::repository::dependency_model::RepositoryRequirementKind,
    ),
    Deconfigure(ObservedPackage),
}

impl InstallChange {
    pub(crate) fn incoming(after: ObservedPackage, before: Option<&Trove>) -> Self {
        match before {
            Some(before) => Self::Update {
                before: ObservedPackage::trove(before),
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
        Ok(ObservedPackage::trove(
            &Trove::find_by_id(conn, id)?.context("planned relation package disappeared")?,
        ))
    };
    let mut changes = Vec::new();
    for removal in &plan.removals {
        changes.push(InstallChange::Remove(
            identity(removal.trove_id)?,
            removal.kind,
        ));
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
    pub outcome: super::InstallOutcome,
    pub planned: Vec<InstallChange>,
    pub commits: Vec<InstallCommit>,
    pub projection: Option<std::sync::Arc<super::preview::PreviewDatabase>>,
}

impl InstallReport {
    /// Count committed requested identities, excluding dependency-only mutations.
    /// An absent repository CCS release is an unspecified selection constraint;
    /// a conversion may supply that release at the verified artifact boundary.
    pub(crate) fn applied_targets(
        &self,
        targets: &std::collections::HashSet<PackageIdentity>,
    ) -> usize {
        self.commits
            .iter()
            .flat_map(|commit| &commit.changes)
            .filter_map(|change| match change {
                InstallChange::Install(after) | InstallChange::Update { after, .. }
                    if targets.iter().any(|target| {
                        target.name == after.identity.name
                            && target.version == after.identity.version
                            && target.version_scheme == after.identity.version_scheme
                            && target.architecture == after.identity.architecture
                            && super::conversion::selected_ccs_release_matches(
                                after.identity.release.as_deref(),
                                target.release.as_deref(),
                            )
                    }) =>
                {
                    Some(&after.identity)
                }
                _ => None,
            })
            .collect::<std::collections::HashSet<_>>()
            .len()
    }

    pub(crate) fn extend(&mut self, other: Self) {
        if other.outcome == super::InstallOutcome::Cancelled {
            self.outcome = other.outcome;
        }
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
            .map(|id| Trove::find_by_id(conn, id)?.context("planned upgrade package disappeared"))
            .transpose()?;
        changes.push(InstallChange::incoming(
            ObservedPackage::prepared(
                PackageIdentity {
                    name: package.name.clone(),
                    version: package.version.clone(),
                    version_scheme: package.semantics.version_scheme,
                    release: package.package_release.clone(),
                    architecture: package.architecture.clone(),
                },
                package.semantics,
            ),
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

#[cfg(all(test, feature = "test-hooks"))]
mod tests;

#[cfg(test)]
mod identity_tests {
    use super::*;
    use conary_core::repository::versioning::VersionScheme;

    #[test]
    fn applied_count_excludes_dependencies_and_respects_selected_identity_constraints() {
        let installed = PackageIdentity {
            name: "requested".into(),
            version: "2.0.0".into(),
            version_scheme: VersionScheme::Conary,
            release: Some("7".into()),
            architecture: Some("x86_64".into()),
        };
        let mut dependency = installed.clone();
        dependency.name = "dependency".into();
        let report = InstallReport {
            outcome: Default::default(),
            projection: None,
            planned: Vec::new(),
            commits: vec![InstallCommit {
                changes: vec![
                    InstallChange::Install(ObservedPackage {
                        identity: installed.clone(),
                        source_format: Some(SourcePackageFormat::Ccs),
                    }),
                    // More observations of one identity must not inflate the count.
                    InstallChange::Install(ObservedPackage {
                        identity: installed.clone(),
                        source_format: None,
                    }),
                    InstallChange::Install(ObservedPackage {
                        identity: dependency,
                        source_format: Some(SourcePackageFormat::Ccs),
                    }),
                ],
                changeset_id: 42,
                file_records: 0,
                publication: None,
            }],
        };
        for (release, scheme, architecture, expected) in [
            (None, VersionScheme::Conary, "x86_64", 1),
            (Some("7"), VersionScheme::Conary, "x86_64", 1),
            (Some("8"), VersionScheme::Conary, "x86_64", 0),
            (Some("7"), VersionScheme::Rpm, "x86_64", 0),
            (Some("7"), VersionScheme::Conary, "aarch64", 0),
        ] {
            let target = PackageIdentity {
                release: release.map(str::to_owned),
                version_scheme: scheme,
                architecture: Some(architecture.into()),
                ..installed.clone()
            };
            assert_eq!(
                report.applied_targets(&std::collections::HashSet::from([target])),
                expected
            );
        }
    }

    #[test]
    fn source_observation_uses_prepared_kind_without_changing_identity() {
        use conary_core::packages::PackageFormatType;

        for (format, source) in [
            (PackageFormatType::Rpm, SourcePackageFormat::Rpm),
            (PackageFormatType::Deb, SourcePackageFormat::Debian),
            (PackageFormatType::Arch, SourcePackageFormat::Alpm),
            (PackageFormatType::Eopkg, SourcePackageFormat::Eopkg),
        ] {
            let semantics = super::super::InstallSemantics::native_package(format);
            let identity = PackageIdentity {
                name: "typed-source".into(),
                version: "1.0".into(),
                version_scheme: semantics.version_scheme,
                release: Some("1".into()),
                architecture: Some("x86_64".into()),
            };
            let native = ObservedPackage::prepared(identity.clone(), semantics);
            let ccs = ObservedPackage::prepared(
                identity.clone(),
                super::super::InstallSemantics::ccs(semantics.version_scheme),
            );
            assert_eq!(native.source_format, Some(source));
            assert_eq!(ccs.source_format, Some(SourcePackageFormat::Ccs));
            assert_eq!(native.identity, identity);
            assert_eq!(ccs.identity, identity);
        }
    }

    #[test]
    fn stored_identity_does_not_infer_a_source_observation() {
        let trove = Trove::new(
            "ccs-rpm-named-package".into(),
            "1.0".into(),
            conary_core::db::models::TroveType::Package,
            VersionScheme::Rpm,
        );
        let observed = ObservedPackage::trove(&trove);
        assert_eq!(observed.source_format, None);
        assert_eq!(observed.identity, PackageIdentity::trove(&trove));
    }

    #[test]
    fn stored_exact_native_identity_retains_its_source_observation() {
        let mut trove = Trove::new(
            "native-package".into(),
            "1.0-1".into(),
            conary_core::db::models::TroveType::Package,
            VersionScheme::Rpm,
        );
        trove.architecture = Some("x86_64".into());
        trove.native_package_identity = Some(
            conary_core::packages::InstalledPackageIdentity::rpm(
                "native-package-1.0-1.x86_64",
                "native-package",
                None,
                "1.0",
                "1",
                "x86_64",
            )
            .unwrap(),
        );
        let observed = ObservedPackage::trove(&trove);
        assert_eq!(observed.source_format, Some(SourcePackageFormat::Rpm));
        assert_eq!(observed.identity, PackageIdentity::trove(&trove));
    }
}
