// apps/conary/src/commands/install/payload_effects.rs

//! Side-effect-free plan for one element's selected-root payload effects.
//!
//! Execution and (later) the event-time projection must derive the same
//! filesystem outcome from one effect plan. Nothing here mutates the selected
//! root, the CAS, or the database: it resolves incoming nodes, runs the typed
//! ownership preflight, and evaluates the source-format config policy.
//!
//! The plan can be built from the extraction-form payload before
//! `store_install_files_in_cas` runs as well as from the stored form execution
//! applies. The two forms carry identical paths, nodes, and content authority;
//! they differ only in where regular bytes are reopened (the package source
//! versus the CAS object), and the derived CAS identity is the same SHA-256 in
//! both cases.
//!
//! The plan has two typed outcomes selected by [`PlanIdentityMode`]:
//! [`ElementPayloadEffects`] is the authoritative apply/mutation plan, and
//! [`ProjectedPayloadEffects`] is the event-time interpreter projection. The
//! projection tolerates owners a pre-payload lifecycle event has not created
//! yet; the authoritative plan refuses them. The two types cannot be
//! interchanged: no apply, mutation, or `capture_install` boundary accepts a
//! [`ProjectedPayloadEffects`].

use super::config_files::{ConfigInstallDecisionRecord, source_for_semantics};
use super::execute;
use super::inner::{self, ResolvedInstallFile, StoredInstallFile};
use super::payload_identity::{IdentityKind, PlanIdentityMode, ProjectedPayloadNode};
use super::shared_directory::DirectoryInstallPlan;
use crate::commands::LiveRootFile;
use anyhow::{Result, bail};
use conary_core::db::models::{FileEntry, PayloadClaim, Trove};
use conary_core::filesystem::{CasStore, ProjectedNode};
use conary_core::packages::PackageFormat;
use conary_core::packages::config_authority::SourceConfigDeclaration;
use conary_core::packages::payload::PackagePayloadFile;
use conary_core::payload::{PayloadContentAuthority, PayloadNodeKind};
use conary_core::transaction::PackageRelationRemoval;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

/// The incoming files a plan is derived from.
///
/// [`Self::Extracted`] is the pre-mutation form the interpreter preflight can
/// read; [`Self::Stored`] is the post-storage form execution applies.
pub(super) enum PayloadEffectFiles<'a> {
    Extracted(&'a [PackagePayloadFile]),
    Stored {
        cas: &'a CasStore,
        files: &'a [StoredInstallFile],
    },
}

impl PayloadEffectFiles<'_> {
    fn resolve(
        &self,
        root: &Path,
        semantics: super::InstallSemantics,
    ) -> Result<Vec<ResolvedInstallFile>> {
        match self {
            Self::Extracted(files) => {
                inner::resolve_extracted_install_files(root, files, semantics)
            }
            Self::Stored { files, .. } => {
                inner::resolve_stored_install_files(root, files, semantics)
            }
        }
    }

    /// Resolve the extraction or stored form under the event-time projection.
    fn project(
        &self,
        root: &Path,
        semantics: super::InstallSemantics,
    ) -> Result<Vec<ProjectedInstallFile>> {
        match self {
            Self::Extracted(files) => project_extracted_install_files(root, files, semantics),
            Self::Stored { files, .. } => project_stored_install_files(root, files, semantics),
        }
    }

    fn live_root_files(&self, resolved: &[ResolvedInstallFile]) -> Result<Vec<LiveRootFile>> {
        match self {
            Self::Extracted(files) => {
                execute::live_root_files_from_extracted_files(files, resolved)
            }
            Self::Stored { cas, .. } => execute::live_root_files_from_stored_files(cas, resolved),
        }
    }
}

/// One incoming payload file whose owners may still be pending.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectedInstallFile {
    path: String,
    node: ProjectedPayloadNode,
    content: Option<PayloadContentAuthority>,
    cas_hash: Option<String>,
}

fn project_extracted_install_files(
    root: &Path,
    extracted_files: &[PackagePayloadFile],
    semantics: super::InstallSemantics,
) -> Result<Vec<ProjectedInstallFile>> {
    require_unique_paths(extracted_files.iter().map(|file| file.path.as_str()))?;
    let projected_nodes = super::payload_identity::project_payload_nodes(
        root,
        extracted_files.iter().map(|file| file.node.clone()),
        semantics,
    )?;
    Ok(extracted_files
        .iter()
        .zip(projected_nodes)
        .map(|(file, node)| ProjectedInstallFile {
            path: file.path.clone(),
            node,
            content: file.content_authority.clone(),
            cas_hash: inner::extracted_cas_identity(file),
        })
        .collect())
}

fn project_stored_install_files(
    root: &Path,
    stored_files: &[StoredInstallFile],
    semantics: super::InstallSemantics,
) -> Result<Vec<ProjectedInstallFile>> {
    require_unique_paths(stored_files.iter().map(|file| file.path.as_str()))?;
    let projected_nodes = super::payload_identity::project_payload_nodes(
        root,
        stored_files.iter().map(|file| file.node.clone()),
        semantics,
    )?;
    Ok(stored_files
        .iter()
        .zip(projected_nodes)
        .map(|(file, node)| ProjectedInstallFile {
            path: file.path.clone(),
            node,
            content: file.content.clone(),
            cas_hash: file.cas_hash.clone(),
        })
        .collect())
}

fn require_unique_paths<'a>(paths: impl IntoIterator<Item = &'a str>) -> Result<()> {
    let mut seen = HashSet::new();
    for path in paths {
        if !seen.insert(path) {
            bail!("payload path {path} is declared more than once");
        }
    }
    Ok(())
}

/// Everything that decides the selected-root filesystem effect of applying one
/// element's payload.
#[derive(Debug, Clone)]
pub(super) struct ElementPayloadEffects {
    /// Incoming files with resolved ownership and exact content authority.
    pub(super) resolved_files: Vec<ResolvedInstallFile>,
    /// Incoming config candidates: every package file whose leaf the directory
    /// plan does not preserve. `capture_install` consumes this set unchanged.
    pub(super) config_candidates: Vec<LiveRootFile>,
    /// Directory ownership and materialization decisions.
    pub(super) directory_plan: DirectoryInstallPlan,
    /// Typed source-format decision for each config payload, in evaluation order.
    pub(super) config_decisions: Vec<ConfigInstallDecisionRecord>,
    /// Files to install at their primary or suffixed paths.
    pub(super) install_files: Vec<LiveRootFile>,
    /// Preserved hardlink targets referenced by `install_files`.
    pub(super) hardlink_references: Vec<LiveRootFile>,
    /// Directory nodes materialized through an existing selected-root symlink.
    pub(super) through_symlink_files: Vec<LiveRootFile>,
    /// Paths to remove.
    pub(super) remove_paths: Vec<String>,
}

impl PartialEq for ElementPayloadEffects {
    fn eq(&self, other: &Self) -> bool {
        self.resolved_files == other.resolved_files
            && live_root_files_eq(&self.config_candidates, &other.config_candidates)
            && self.directory_plan == other.directory_plan
            && self.config_decisions == other.config_decisions
            && live_root_files_eq(&self.install_files, &other.install_files)
            && live_root_files_eq(&self.hardlink_references, &other.hardlink_references)
            && live_root_files_eq(&self.through_symlink_files, &other.through_symlink_files)
            && self.remove_paths == other.remove_paths
    }
}

impl Eq for ElementPayloadEffects {}

impl ElementPayloadEffects {
    /// The files this plan materializes into the selected root, at their
    /// effective paths.
    ///
    /// Preserved leaves are excluded: the selected root already owns them, so
    /// the event-time projection must resolve them on disk rather than overlay
    /// a node the payload never writes.
    pub(super) fn materialized_files(&self) -> impl Iterator<Item = &LiveRootFile> {
        self.install_files
            .iter()
            .chain(self.through_symlink_files.iter())
            .filter(|file| !self.directory_plan.preserves_leaf(&file.path))
    }

    /// The typed overlay this plan materializes into the selected root.
    ///
    /// This is the node-kind derivation the event-time interpreter projection
    /// overlays, so preflight and execution agree on what the payload writes.
    pub(super) fn projected_nodes(&self) -> BTreeMap<String, ProjectedNode> {
        self.materialized_files()
            .map(|file| {
                (
                    file.path.clone(),
                    projected_node(&file.node.source.kind, file.node.source.mode),
                )
            })
            .collect()
    }
}

/// The event-time projection of one element's payload.
///
/// This is a distinct type from [`ElementPayloadEffects`] precisely so a plan
/// whose owners may be pending can never be passed to an apply or mutation
/// boundary. Those boundaries require [`ElementPayloadEffects`], which is built
/// only under [`PlanIdentityMode::Authoritative`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProjectedPayloadEffects {
    nodes: BTreeMap<String, ProjectedNode>,
    incoming_paths: BTreeSet<String>,
    pending_owners: BTreeSet<PendingOwner>,
}

/// One named owner a pre-payload lifecycle event must define before execution.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct PendingOwner {
    pub kind: IdentityKind,
    pub name: String,
}

impl ProjectedPayloadEffects {
    /// The typed overlay this plan materializes into the selected root.
    pub(super) fn projected_nodes(&self) -> BTreeMap<String, ProjectedNode> {
        self.nodes.clone()
    }

    /// Every incoming payload path, including preserved leaves.
    pub(super) fn incoming_paths(&self) -> impl Iterator<Item = &str> {
        self.incoming_paths.iter().map(String::as_str)
    }

    /// The named owners a pre-payload lifecycle event must define.
    pub(super) fn pending_owners(&self) -> impl Iterator<Item = &PendingOwner> {
        self.pending_owners.iter()
    }
}

/// Whether a pending owner leaves a path's projected kind, mode, or path
/// unknowable.
///
/// [`super::shared_directory::preflight_resolved_file_ownership`] compares an
/// incoming node with an existing path's claim through
/// `PayloadSharingPolicy::compare` to decide whether a shared payload is
/// preserved, and consults the selected root for directory materialization. A
/// pending owner has no resolved numeric identity, so a path that already has
/// database or selected-root authority resolves on disk instead of being
/// overlaid. Declared config payloads take the same route because the
/// projection does not recompute their suffix decision for a pending owner.
/// Directory-alias decisions such as `rpm_symlink_owner_may_apply` read only
/// the already-resolved selected-root node, so they are unaffected. The refusal
/// direction is intentional: the interpreter projection may miss an overlay
/// execution would eventually materialize, never the reverse.
fn pending_owner_decision_is_unknown(
    conn: &rusqlite::Connection,
    selected_root: &Path,
    file: &ProjectedInstallFile,
    declared_configs: &BTreeSet<&str>,
) -> Result<bool> {
    if declared_configs.contains(file.path.as_str())
        || conary_core::config_transaction::is_etc_config_payload(
            &file.path,
            &file.node.source.kind,
        )
    {
        return Ok(true);
    }
    if FileEntry::find_by_path(conn, &file.path)?.is_some() {
        return Ok(true);
    }
    if !PayloadClaim::find_by_path(conn, &file.path)?.is_empty() {
        return Ok(true);
    }
    // An ancestor symlink (for example a usr-merge alias) can move the
    // effective spelling even when the target leaf is absent, so the declared
    // path is not the projected path.
    if conary_core::filesystem::selected_root::selected_root_effective_package_path(
        selected_root,
        &file.path,
    )? != file.path
    {
        return Ok(true);
    }
    Ok(
        conary_core::filesystem::selected_root::capture_selected_root_node(
            selected_root,
            &file.path,
        )?
        .is_some(),
    )
}

/// Map one payload node to the node the selected-root projection overlays.
///
/// This is the single node-kind derivation shared by execution's
/// [`ElementPayloadEffects`] and the native event-time projection.
pub(super) fn projected_node(kind: &PayloadNodeKind, mode: u32) -> ProjectedNode {
    match kind {
        PayloadNodeKind::Regular { .. } => ProjectedNode::Regular {
            executable: mode & 0o111 != 0,
        },
        PayloadNodeKind::Symlink { target } => ProjectedNode::Symlink {
            target: target.clone(),
        },
        PayloadNodeKind::Hardlink { target, .. } => ProjectedNode::Hardlink {
            target: target.clone(),
        },
        PayloadNodeKind::Directory => ProjectedNode::Directory,
        PayloadNodeKind::BlockDevice { .. }
        | PayloadNodeKind::CharacterDevice { .. }
        | PayloadNodeKind::Fifo
        | PayloadNodeKind::Socket => ProjectedNode::Other,
    }
}

/// The typed overlay of one element's declared payload files.
///
/// This is the declared-spelling projection used where no selected-root effects
/// plan exists (the CCS dry run and state restore). Install and batch callers
/// overlay [`ProjectedPayloadEffects::projected_nodes`] instead, so config
/// suffixes and preserved aliases match what execution materializes.
pub(super) fn projected_payload_nodes(
    files: &[PackagePayloadFile],
) -> BTreeMap<String, ProjectedNode> {
    files
        .iter()
        .map(|file| {
            (
                file.path.clone(),
                projected_node(&file.node.kind, file.node.mode),
            )
        })
        .collect()
}

/// Compare two planned file lists by the typed effect each file carries.
///
/// `LiveRootContent` holds a reopenable source, which is a byte-transport
/// detail rather than part of the filesystem effect; the plan owns the exact
/// content authority that determines what is written.
fn live_root_files_eq(left: &[LiveRootFile], right: &[LiveRootFile]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(left, right)| {
            left.path == right.path
                && left.node == right.node
                && left.content.authority() == right.content.authority()
        })
}

/// Typed inputs for [`plan_element_payload_effects`] and
/// [`plan_element_payload_projection`].
pub(super) struct ElementPayloadEffectInput<'a> {
    pub(super) semantics: super::InstallSemantics,
    pub(super) package_name: &'a str,
    pub(super) relation_removals: &'a [PackageRelationRemoval],
    pub(super) replacing_trove_id: Option<i64>,
    pub(super) config_declarations: &'a [SourceConfigDeclaration],
    pub(super) files: PayloadEffectFiles<'a>,
    /// Which owner-resolution contract this plan is built under. The
    /// authoritative planner accepts only [`PlanIdentityMode::Authoritative`];
    /// the projection planner accepts only [`PlanIdentityMode::EventProjection`].
    pub(super) identity_mode: PlanIdentityMode,
}

/// Compute one element's authoritative selected-root payload effects without
/// mutating anything.
///
/// This is the single authority behind execution's `apply_payload` and
/// `capture_install`. Callers that project an interpreter before execution run
/// [`plan_element_payload_projection`] instead, which returns the distinct
/// [`ProjectedPayloadEffects`] type.
#[allow(clippy::too_many_arguments)]
fn plan_resolved_payload_effects(
    conn: &rusqlite::Connection,
    selected_root: &Path,
    semantics: super::InstallSemantics,
    package_name: &str,
    relation_removals: &[PackageRelationRemoval],
    replacing_trove_id: Option<i64>,
    config_declarations: &[SourceConfigDeclaration],
    resolved_files: &[ResolvedInstallFile],
    all_package_files: &[LiveRootFile],
) -> Result<ElementPayloadEffects> {
    let directory_plan = inner::preflight_resolved_file_ownership(
        conn,
        selected_root,
        resolved_files,
        package_name,
        relation_removals,
        semantics,
    )?;
    let config_candidates = all_package_files
        .iter()
        .filter(|file| !directory_plan.preserves_leaf(&file.path))
        .cloned()
        .collect::<Vec<_>>();
    let through_symlink_files = directory_plan.through_symlink_root_files(resolved_files);
    let mut config_plan = super::config_files::prepare_config_install(
        conn,
        selected_root,
        source_for_semantics(semantics),
        config_declarations,
        replacing_trove_id,
        config_candidates.clone(),
    )?;
    let hardlink_references = execute::prepare_preserved_hardlink_references(
        conn,
        &directory_plan,
        all_package_files,
        &mut config_plan.files,
    )?;

    Ok(ElementPayloadEffects {
        resolved_files: resolved_files.to_vec(),
        config_candidates,
        directory_plan,
        config_decisions: config_plan.decisions,
        install_files: config_plan.files,
        hardlink_references,
        through_symlink_files,
        remove_paths: config_plan.remove_paths,
    })
}

/// Compute one element's authoritative selected-root payload effects without
/// mutating anything.
///
/// This is the apply/mutation plan. It requires every named owner to already be
/// defined by the selected root, so its [`ResolvedInstallFile`]s carry
/// authoritative numeric ownership.
pub(super) fn plan_element_payload_effects(
    conn: &rusqlite::Connection,
    selected_root: &Path,
    input: ElementPayloadEffectInput<'_>,
) -> Result<ElementPayloadEffects> {
    let ElementPayloadEffectInput {
        semantics,
        package_name,
        relation_removals,
        replacing_trove_id,
        config_declarations,
        files,
        identity_mode,
    } = input;
    if identity_mode != PlanIdentityMode::Authoritative {
        bail!("authoritative payload effects require Authoritative identity planning");
    }

    let resolved_files = files.resolve(selected_root, semantics)?;
    let all_package_files = files.live_root_files(&resolved_files)?;
    plan_resolved_payload_effects(
        conn,
        selected_root,
        semantics,
        package_name,
        relation_removals,
        replacing_trove_id,
        config_declarations,
        &resolved_files,
        &all_package_files,
    )
}

/// Compute one element's event-time payload projection without mutating
/// anything.
///
/// The projection runs the same directory, config, and effective-path
/// derivation as [`plan_element_payload_effects`] for every file whose owners
/// the selected root already defines. A file with a pending owner is overlaid
/// only when no existing database or selected-root authority makes its
/// projected state depend on that owner; otherwise it resolves on disk, which
/// is the refusing direction for interpreter availability.
pub(super) fn plan_element_payload_projection(
    conn: &rusqlite::Connection,
    selected_root: &Path,
    input: ElementPayloadEffectInput<'_>,
) -> Result<ProjectedPayloadEffects> {
    let ElementPayloadEffectInput {
        semantics,
        package_name,
        relation_removals,
        replacing_trove_id,
        config_declarations,
        files,
        identity_mode,
    } = input;
    if identity_mode != PlanIdentityMode::EventProjection {
        bail!("the event-time projection requires EventProjection identity planning");
    }

    let projected_files = files.project(selected_root, semantics)?;
    let declared_configs = config_declarations
        .iter()
        .map(|declaration| declaration.path())
        .collect::<BTreeSet<_>>();
    // A hardlink whose target is itself pending has a projected topology that
    // depends on the pending owner's path, so it joins the pending set rather
    // than the authoritative pipeline.
    let pending_paths = projected_files
        .iter()
        .filter(|file| file.node.resolved().is_none())
        .map(|file| file.path.clone())
        .collect::<BTreeSet<_>>();
    let mut incoming_paths = BTreeSet::new();
    let mut pending_owners = BTreeSet::new();
    let mut resolved_files = Vec::new();
    let mut extracted_subset = Vec::new();
    let mut pending_files = Vec::new();
    for (index, file) in projected_files.iter().enumerate() {
        incoming_paths.insert(file.path.clone());
        for (kind, name) in file.node.pending_owners() {
            pending_owners.insert(PendingOwner {
                kind,
                name: name.to_string(),
            });
        }
        let hardlink_to_pending = matches!(
            &file.node.source.kind,
            PayloadNodeKind::Hardlink { target, .. } if pending_paths.contains(target)
        );
        let Some(node) = file.node.resolved().filter(|_| !hardlink_to_pending) else {
            pending_files.push(file);
            continue;
        };
        resolved_files.push(ResolvedInstallFile {
            path: file.path.clone(),
            node,
            content: file.content.clone(),
            cas_hash: file.cas_hash.clone(),
        });
        if let PayloadEffectFiles::Extracted(extracted) = &files {
            extracted_subset.push(extracted[index].clone());
        }
    }
    let all_package_files = match &files {
        PayloadEffectFiles::Extracted(_) => {
            execute::live_root_files_from_extracted_files(&extracted_subset, &resolved_files)?
        }
        PayloadEffectFiles::Stored { cas, .. } => {
            execute::live_root_files_from_stored_files(cas, &resolved_files)?
        }
    };
    let effects = plan_resolved_payload_effects(
        conn,
        selected_root,
        semantics,
        package_name,
        relation_removals,
        replacing_trove_id,
        config_declarations,
        &resolved_files,
        &all_package_files,
    )?;
    let mut nodes = effects.projected_nodes();
    for file in pending_files {
        if pending_owner_decision_is_unknown(conn, selected_root, file, &declared_configs)? {
            continue;
        }
        nodes.insert(
            file.path.clone(),
            projected_node(&file.node.source.kind, file.node.source.mode),
        );
    }
    let plan = ProjectedPayloadEffects {
        nodes,
        incoming_paths,
        pending_owners,
    };
    let pending = plan.pending_owners().count();
    if pending > 0 {
        tracing::debug!(
            package = package_name,
            pending,
            "payload identity projection deferred owners to pre-payload lifecycle events"
        );
    }
    Ok(plan)
}

/// Plan one element's effects from its extraction-form payload.
///
/// Interpreter preflight runs before `store_install_files_in_cas`, so callers
/// build the plan from exactly what they hold at that point: the parsed package,
/// its extracted files, the committed database, and the selected root.
pub(super) fn plan_extracted_element_payload_effects(
    conn: &rusqlite::Connection,
    selected_root: &Path,
    pkg: &dyn PackageFormat,
    extracted_files: &[PackagePayloadFile],
    semantics: super::InstallSemantics,
    old_trove: Option<&Trove>,
    relation_removals: &[PackageRelationRemoval],
) -> Result<ElementPayloadEffects> {
    let config_declarations = pkg.config_declarations()?;
    plan_element_payload_effects(
        conn,
        selected_root,
        ElementPayloadEffectInput {
            semantics,
            package_name: pkg.name(),
            relation_removals,
            replacing_trove_id: old_trove.and_then(|trove| trove.id),
            config_declarations: &config_declarations,
            files: PayloadEffectFiles::Extracted(extracted_files),
            identity_mode: PlanIdentityMode::Authoritative,
        },
    )
}

/// Plan one element's event-time projection from its extraction-form payload.
///
/// This is the pre-mutation counterpart of
/// [`plan_extracted_element_payload_effects`]: it tolerates named owners a
/// pre-payload lifecycle event has not created yet.
pub(super) fn plan_extracted_element_payload_projection(
    conn: &rusqlite::Connection,
    selected_root: &Path,
    pkg: &dyn PackageFormat,
    extracted_files: &[PackagePayloadFile],
    semantics: super::InstallSemantics,
    old_trove: Option<&Trove>,
    relation_removals: &[PackageRelationRemoval],
) -> Result<ProjectedPayloadEffects> {
    let config_declarations = pkg.config_declarations()?;
    plan_element_payload_projection(
        conn,
        selected_root,
        ElementPayloadEffectInput {
            semantics,
            package_name: pkg.name(),
            relation_removals,
            replacing_trove_id: old_trove.and_then(|trove| trove.id),
            config_declarations: &config_declarations,
            files: PayloadEffectFiles::Extracted(extracted_files),
            identity_mode: PlanIdentityMode::EventProjection,
        },
    )
}

#[cfg(test)]
#[path = "payload_effects/tests.rs"]
mod tests;
