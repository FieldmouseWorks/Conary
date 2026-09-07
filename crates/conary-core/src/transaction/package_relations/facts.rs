// crates/conary-core/src/transaction/package_relations/facts.rs

//! Transaction-scoped borrowed facts for relation and deconfiguration planning.

use std::collections::BTreeMap;

use crate::repository::package_relation::{PackageRelationCandidate, PackageRelationProvide};
use crate::repository::versioning::VersionScheme;

use super::{IncomingPackageRelations, InstalledCandidate};

/// Own each capability-view array once. Installed rows and incoming archive
/// facts remain the owners of every name and version string.
pub(super) struct RelationFacts<'a> {
    installed: BTreeMap<i64, ProjectedCandidate<'a>>,
    incoming: Vec<ProjectedCandidate<'a>>,
}

impl<'a> RelationFacts<'a> {
    pub(super) fn new(
        installed: &'a [InstalledCandidate],
        incoming: &[IncomingPackageRelations<'a>],
    ) -> Self {
        Self {
            installed: installed
                .iter()
                .map(|package| {
                    (
                        package.trove.id.expect("installed trove has id"),
                        ProjectedCandidate {
                            name: &package.trove.name,
                            version: &package.trove.version,
                            version_scheme: package.version_scheme,
                            provides: package
                                .provides
                                .iter()
                                .map(|provide| PackageRelationProvide {
                                    name: &provide.name,
                                    version: provide.version.as_deref(),
                                    version_scheme: package.version_scheme,
                                })
                                .collect(),
                        },
                    )
                })
                .collect(),
            incoming: incoming
                .iter()
                .map(|package| ProjectedCandidate {
                    name: package.name,
                    version: package.version,
                    version_scheme: package.version_scheme,
                    provides: package
                        .provides
                        .iter()
                        .map(|provide| PackageRelationProvide {
                            name: &provide.name,
                            version: provide.version.as_deref(),
                            version_scheme: provide.version_scheme,
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    pub(super) fn installed(&self, package: &InstalledCandidate) -> PackageRelationCandidate<'_> {
        self.installed[&package.trove.id.expect("installed trove has id")].candidate()
    }

    /// Preserve the transaction's original incoming indices in every subset.
    pub(super) fn incoming(&self) -> impl Iterator<Item = PackageRelationCandidate<'_>> {
        self.incoming.iter().map(ProjectedCandidate::candidate)
    }
}

struct ProjectedCandidate<'a> {
    name: &'a str,
    version: &'a str,
    version_scheme: VersionScheme,
    provides: Vec<PackageRelationProvide<'a>>,
}

impl ProjectedCandidate<'_> {
    fn candidate(&self) -> PackageRelationCandidate<'_> {
        PackageRelationCandidate {
            name: self.name,
            version: self.version,
            version_scheme: self.version_scheme,
            provides: &self.provides,
        }
    }
}
