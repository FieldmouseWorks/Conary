// crates/conary-core/src/resolver/sat/end_state/candidates.rs

//! The raw installed hard-group candidate universe and the affected capability
//! set for fixed-point end-state validation.
//!
//! Loading a candidate captures only the surviving package's stored group and
//! architecture option. Architecture authority is resolved and pre-transaction
//! satisfaction is evaluated only when the loop admits a candidate, so an
//! unrelated installed trove with no authority cannot refuse a transaction and
//! unmentioned groups cost nothing.

use std::collections::{HashMap, HashSet};

use rusqlite::Connection;

use crate::db::models::InstalledRequirementGroup;
use crate::error::{Error, Result};
use crate::repository::dependency_model::{
    RepositoryRequirementExpression, RepositoryRequirementKind,
};
use crate::repository::versioning::VersionScheme;
use crate::resolver::canonical::CanonicalEquivalents;
use crate::resolver::identity::PackageIdentity;

use super::{ValidatedGroupOwner, ValidatedRequirementGroup};

/// One surviving installed package's stored hard group before admission, held
/// with its owning package's identity and architecture option.
#[derive(Clone)]
pub(super) struct InstalledCandidate {
    pub(super) expression: RepositoryRequirementExpression,
    pub(super) native_text: Option<String>,
    pub(super) version_scheme: VersionScheme,
    pub(super) trove_id: i64,
    pub(super) package_name: String,
    /// The owning package's stored architecture authority, when it has one.
    pub(super) architecture: Option<String>,
}

impl InstalledCandidate {
    /// Resolve an admitted candidate's architecture authority into a group the
    /// evaluator can use.
    ///
    /// A missing authority is a typed refusal for this admitted group only, so
    /// the error names this candidate's owning package.
    pub(super) fn into_validated(self) -> Result<ValidatedRequirementGroup> {
        let depending_architecture = self
            .architecture
            .as_deref()
            .filter(|architecture| !architecture.is_empty())
            .ok_or_else(|| {
                Error::ConfigError(format!(
                    "installed dependent '{}' has no architecture authority",
                    self.package_name
                ))
            })?
            .to_string();
        Ok(ValidatedRequirementGroup {
            expression: self.expression,
            native_text: self.native_text,
            version_scheme: self.version_scheme,
            depending_architecture,
            owner: ValidatedGroupOwner::Installed {
                trove_id: self.trove_id,
                package_name: self.package_name,
            },
        })
    }
}

/// Load every surviving installed package's `Depends`/`PreDepends` group as a
/// cheap raw candidate in deterministic persisted order.
///
/// Only groups whose owning trove is a surviving package are candidates; an
/// outgoing trove is not part of the end state. Loading resolves no
/// architecture authority and evaluates nothing.
pub(super) fn installed_hard_group_candidates(
    conn: &Connection,
    outgoing_trove_ids: &[i64],
    before: &[PackageIdentity],
) -> Result<Vec<InstalledCandidate>> {
    let outgoing = outgoing_trove_ids.iter().copied().collect::<HashSet<_>>();
    let mut packages = HashMap::new();
    for package in before {
        let Some(trove_id) = package.installed_trove_id else {
            continue;
        };
        if outgoing.contains(&trove_id) {
            continue;
        }
        packages.insert(trove_id, package);
    }

    let mut candidates = Vec::new();
    for stored in InstalledRequirementGroup::list_all(conn)? {
        if !matches!(
            stored.kind,
            RepositoryRequirementKind::Depends | RepositoryRequirementKind::PreDepends
        ) {
            continue;
        }
        let Some(package) = packages.get(&stored.trove_id) else {
            continue;
        };
        candidates.push(InstalledCandidate {
            expression: stored.requirement.expression.clone(),
            native_text: stored.requirement.native_text.clone(),
            version_scheme: stored.version_scheme,
            trove_id: stored.trove_id,
            package_name: package.name.clone(),
            architecture: package.architecture.clone(),
        });
    }
    Ok(candidates)
}

/// Insert a package identity name and every canonical equivalent of that name
/// into the affected set.
///
/// Canonical equivalence is identity-only. Call sites add provided-capability
/// names literally, because a canonical row must never make a capability name
/// affected.
pub(super) fn insert_identity_name(
    affected: &mut HashSet<String>,
    name: &str,
    canonical_equivalents: &CanonicalEquivalents,
) {
    affected.insert(name.to_string());
    for equivalent in canonical_equivalents.for_name(name) {
        affected.insert(equivalent.clone());
    }
}

/// The capability names the transaction already declares as added or removed:
/// the incoming package's name and provides, the incoming groups' atoms (which
/// the solve may install from a repository), and every declared outgoing
/// trove's name and provides. The loop extends this with the identities SAT
/// selects and the troves relation planning removes.
///
/// A package identity name contributes its canonical equivalents too, because
/// removing or replacing one implementation removes the canonical identity and
/// can break an installed dependent that names a sibling. Provided-capability
/// names stay literal.
pub(super) fn affected_capability_names(
    outgoing_trove_ids: &[i64],
    incoming: Option<&PackageIdentity>,
    incoming_groups: &[ValidatedRequirementGroup],
    before: &[PackageIdentity],
    canonical_equivalents: &CanonicalEquivalents,
) -> HashSet<String> {
    let mut affected = HashSet::new();
    if let Some(incoming) = incoming {
        insert_identity_name(&mut affected, &incoming.name, canonical_equivalents);
        for capability in &incoming.provided_capabilities {
            affected.insert(capability.name.clone());
        }
    }
    for group in incoming_groups {
        for atom in group.expression.atoms() {
            affected.insert(atom.name.clone());
        }
    }
    let outgoing = outgoing_trove_ids.iter().copied().collect::<HashSet<_>>();
    for package in before {
        if !package
            .installed_trove_id
            .is_some_and(|trove_id| outgoing.contains(&trove_id))
        {
            continue;
        }
        insert_identity_name(&mut affected, &package.name, canonical_equivalents);
        for capability in &package.provided_capabilities {
            affected.insert(capability.name.clone());
        }
    }
    affected
}
