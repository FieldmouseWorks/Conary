// apps/conary/src/commands/install/ccs_hook_interpreter.rs
//! Transaction-ordered availability of CCS hook interpreters.
//!
//! Preflight replays every element's planned payload effects into one
//! [`SelectedRootProjection`], the same resolver execution uses, then requires
//! each hook interpreter against that final projected state.

use super::payload_effects::ElementPayloadEffects;
use super::{ExtractionResult, InstallSemantics};
use anyhow::Context;
use conary_core::ccs::manifest::Hooks;
use conary_core::db::models::{PackagePayloadOwnership, PayloadClaim, Trove};
use conary_core::filesystem::{ProjectedExecutable, SelectedRootProjection};
use conary_core::packages::PackageFormat;
use conary_core::transaction::PackageRelationRemoval;
use rusqlite::Connection;
use std::collections::BTreeSet;
use std::path::Path;

/// One transaction element's payload boundary and hook interpreters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ElementPlan {
    package: String,
    version: String,
    /// Installed troves this element removes (old version, relation
    /// removals, or restore removals). Their paths are resolved claim-aware at
    /// preflight, against every trove the whole transaction removes.
    removed_trove_ids: Vec<i64>,
    /// Paths this element removes that are already resolved.
    removed_paths: Vec<String>,
    /// The element's side-effect-free selected-root payload plan. `None` for a
    /// removal-only element, which introduces no payload.
    effects: Option<ElementPayloadEffects>,
    hook_interpreters: Vec<HookInterpreter>,
}

/// Build one element plan from its pre-mutation payload effects.
pub(super) fn element_plan(
    package: &str,
    version: &str,
    old_trove: Option<&Trove>,
    relation_removals: &[PackageRelationRemoval],
    effects: ElementPayloadEffects,
    hook_interpreters: Vec<HookInterpreter>,
) -> ElementPlan {
    let removed_trove_ids = old_trove
        .and_then(|trove| trove.id)
        .into_iter()
        .chain(relation_removals.iter().map(|removal| removal.trove_id))
        .collect();
    ElementPlan {
        package: package.to_string(),
        version: version.to_string(),
        removed_trove_ids,
        removed_paths: Vec::new(),
        effects: Some(effects),
        hook_interpreters,
    }
}

/// Plan one element's payload effects from its extraction form, then record its
/// hook interpreters.
///
/// This is the shared construction path for pre-mutation callers that hold the
/// parsed package and its extracted payload rather than an existing plan.
#[allow(clippy::too_many_arguments)]
pub(super) fn extracted_element_plan(
    conn: &Connection,
    root: &Path,
    pkg: &dyn PackageFormat,
    extraction: &ExtractionResult,
    semantics: InstallSemantics,
    old_trove: Option<&Trove>,
    relation_removals: &[PackageRelationRemoval],
    hook_interpreters: Vec<HookInterpreter>,
) -> anyhow::Result<ElementPlan> {
    let effects = super::payload_effects::plan_extracted_element_payload_effects(
        conn,
        root,
        pkg,
        &extraction.extracted_files,
        semantics,
        old_trove,
        relation_removals,
    )?;
    Ok(element_plan(
        pkg.name(),
        pkg.version(),
        old_trove,
        relation_removals,
        effects,
        hook_interpreters,
    ))
}

/// One removal-only element: every path the removed troves own leaves the
/// projected state before later elements are recorded.
pub(super) fn removal_element_plan(troves: &[Trove]) -> ElementPlan {
    ElementPlan {
        package: String::new(),
        version: String::new(),
        removed_trove_ids: troves.iter().filter_map(|trove| trove.id).collect(),
        removed_paths: Vec::new(),
        effects: None,
        hook_interpreters: Vec::new(),
    }
}

/// Record every element's payload boundary, then require every element's hook
/// interpreters, each against the transaction's final projected state. Both
/// passes complete before the caller mutates.
pub(super) fn preflight_hook_interpreters(
    conn: &Connection,
    root: &Path,
    elements: &[ElementPlan],
) -> anyhow::Result<()> {
    let transaction_removed = elements
        .iter()
        .flat_map(|element| element.removed_trove_ids.iter().copied())
        .collect::<BTreeSet<_>>();
    let claims = if transaction_removed.is_empty() {
        None
    } else {
        Some(PayloadClaim::index_all(conn)?)
    };
    let mut projection = SelectedRootProjection::new(root);
    for element in elements {
        let mut removed_paths = element.removed_paths.clone();
        if let Some(claims) = claims.as_ref() {
            // #1112 slice 5: claim-aware `released_paths` still diverges from
            // execution for co-claimants, alias removal, and non-empty
            // directories.
            removed_paths.extend(PackagePayloadOwnership::released_paths(
                conn,
                claims,
                &element.removed_trove_ids,
                &transaction_removed,
            )?);
        }
        for path in removed_paths {
            projection.remove(&path)?;
        }
        if let Some(effects) = element.effects.as_ref() {
            for (path, node) in effects.projected_nodes() {
                projection.insert(&path, node)?;
            }
        }
    }
    for element in elements {
        for hook in &element.hook_interpreters {
            require_interpreter(&projection, element, hook)?;
        }
    }
    Ok(())
}

/// Require one element hook's interpreter against the transaction's final
/// projected state.
fn require_interpreter(
    projection: &SelectedRootProjection<'_>,
    element: &ElementPlan,
    hook: &HookInterpreter,
) -> anyhow::Result<()> {
    let resolved = projection
        .resolve_executable(&hook.interpreter)
        .with_context(|| {
            format!(
                "failed to resolve {} interpreter {} for {} {} in the selected root",
                hook.phase, hook.interpreter, element.package, element.version
            )
        })?;
    match resolved {
        ProjectedExecutable::Executable { .. } => Ok(()),
        ProjectedExecutable::NotExecutable { .. } | ProjectedExecutable::Missing => {
            Err(CcsHookInterpreterUnavailable {
                package: element.package.clone(),
                version: element.version.clone(),
                phase: hook.phase,
                interpreter: hook.interpreter.clone(),
            }
            .into())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HookPhase {
    PostInstall,
    PreRemove,
}

impl std::fmt::Display for HookPhase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::PostInstall => "post-install",
            Self::PreRemove => "pre-remove",
        })
    }
}

/// One lifecycle hook interpreter an element must be able to run, tagged with
/// the phase that runs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HookInterpreter {
    pub phase: HookPhase,
    pub interpreter: String,
}

/// Collect the interpreters a manifest's hooks need, in phase order: the
/// post-install hook runs now, and the pre-remove hook runs against the same
/// final projected state once this install completes.
pub(super) fn hook_interpreters(hooks: &Hooks) -> Vec<HookInterpreter> {
    let mut interpreters = Vec::new();
    if let Some(hook) = hooks.post_install.as_ref() {
        interpreters.push(HookInterpreter {
            phase: HookPhase::PostInstall,
            interpreter: hook.interpreter.clone(),
        });
    }
    if let Some(hook) = hooks.pre_remove.as_ref() {
        interpreters.push(HookInterpreter {
            phase: HookPhase::PreRemove,
            interpreter: hook.interpreter.clone(),
        });
    }
    interpreters
}

#[derive(Debug, thiserror::Error)]
#[error(
    "{phase} hook for {package} {version} requires interpreter {interpreter}, but no installed or planned package provides it in the selected root; install a package that provides {interpreter} (the hook declares it as a pre-install requirement) or enroll a repository that supplies one"
)]
pub(super) struct CcsHookInterpreterUnavailable {
    pub package: String,
    pub version: String,
    pub phase: HookPhase,
    pub interpreter: String,
}

#[cfg(test)]
mod tests;
