// crates/conary-core/src/resolver/provider/relation_removal.rs

//! The single removal predicate shared by post-solve relation planning and the
//! forced-installed SAT root compiler.
//!
//! The predicate is deliberately computed from typed relation facts with the
//! same native evaluator the relation planner already uses, so a forced
//! installed root can never disagree with the relation plan about which loaded
//! candidate removes the trove.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::{Error, Result};
use crate::repository::package_relation::{
    PackageRelationCandidate, PackageRelationProvide, relation_matches_candidate,
};

use super::ConaryProvider;
use super::types::{ConaryConstraint, SolverExpression, SolverRelation};

/// Whether one `relation` declared by `replacement_name` relation-removes the
/// installed candidate `existing`.
///
/// Obsolete and Replace are the only relation kinds that remove a matched
/// installed package; Conflict and Breaks are mutual co-installation
/// constraints the planner handles separately. A same-name declaration never
/// relation-removes the installed variant, because an upgrade or replacement
/// names that variant on its own.
pub(crate) fn relation_removes_candidate(
    replacement_name: &str,
    existing: &PackageRelationCandidate<'_>,
    relation: &SolverRelation,
) -> Result<bool> {
    if replacement_name == existing.name || !relation.relation.kind.removes_matching_packages() {
        return Ok(false);
    }
    relation_matches_candidate(&relation.relation, relation.scheme, existing)
        .map_err(Error::ResolutionError)
}

impl ConaryProvider<'_> {
    /// The SAT root keeping installed trove `trove_id` selected unless a loaded
    /// non-installed candidate relation-removes it.
    ///
    /// The installed alternative is the exact trove identity. Each removing
    /// candidate is grouped by its concrete package name into an
    /// `ExactSolvables` set, mirroring the condition compiler's per-name
    /// encoding so resolvo tracks the disjunction per name. When no loaded
    /// candidate removes the trove the root is just the exact installed-trove
    /// atom, so an unloaded obsoleter is never pulled into the solve.
    pub(crate) fn forced_installed_root(&self, trove_id: i64) -> Result<SolverExpression> {
        let existing = self.installed_package_for_trove(trove_id).ok_or_else(|| {
            Error::ResolutionError(format!(
                "forced installed trove {trove_id} is not a loaded solver candidate"
            ))
        })?;
        let provides = existing
            .provided_capabilities
            .iter()
            .map(|capability| PackageRelationProvide {
                name: &capability.name,
                version: capability.version.as_deref(),
                version_scheme: capability.version_scheme,
            })
            .collect::<Vec<_>>();
        let candidate = PackageRelationCandidate {
            name: &existing.name,
            version: &existing.version,
            version_scheme: existing.version_scheme,
            provides: &provides,
        };

        let mut alternatives = vec![SolverExpression::atom(
            existing.name.clone(),
            ConaryConstraint::ExactInstalledTrove(trove_id),
        )];
        let mut removers: BTreeMap<String, BTreeSet<u32>> = BTreeMap::new();
        for candidate_id in self.solvable_ids() {
            let replacement = self.get_solvable(*candidate_id);
            if replacement.installed_trove_id.is_some() {
                continue;
            }
            for relation in self.get_relation_list(*candidate_id)? {
                if relation_removes_candidate(&replacement.name, &candidate, relation)? {
                    removers
                        .entry(replacement.name.clone())
                        .or_default()
                        .insert(candidate_id.into_raw());
                    break;
                }
            }
        }
        for (name, solvables) in removers {
            alternatives.push(SolverExpression::atom(
                name,
                ConaryConstraint::ExactSolvables(solvables),
            ));
        }
        if alternatives.len() == 1 {
            return Ok(alternatives.pop().expect("one forced-root alternative"));
        }
        Ok(SolverExpression::Or(alternatives))
    }
}
