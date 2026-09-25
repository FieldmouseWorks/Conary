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

use super::config_files::{ConfigInstallDecisionRecord, source_for_semantics};
use super::execute;
use super::inner::{self, ResolvedInstallFile, StoredInstallFile};
use super::shared_directory::DirectoryInstallPlan;
use crate::commands::LiveRootFile;
use anyhow::Result;
use conary_core::db::models::Trove;
use conary_core::filesystem::{CasStore, ProjectedNode};
use conary_core::packages::PackageFormat;
use conary_core::packages::config_authority::SourceConfigDeclaration;
use conary_core::packages::payload::PackagePayloadFile;
use conary_core::payload::PayloadNodeKind;
use conary_core::transaction::PackageRelationRemoval;
use std::collections::BTreeMap;
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

    fn live_root_files(&self, resolved: &[ResolvedInstallFile]) -> Result<Vec<LiveRootFile>> {
        match self {
            Self::Extracted(files) => {
                execute::live_root_files_from_extracted_files(files, resolved)
            }
            Self::Stored { cas, .. } => execute::live_root_files_from_stored_files(cas, resolved),
        }
    }
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
/// overlay [`ElementPayloadEffects::projected_nodes`] instead, so config
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

/// Typed inputs for [`plan_element_payload_effects`].
pub(super) struct ElementPayloadEffectInput<'a> {
    pub(super) semantics: super::InstallSemantics,
    pub(super) package_name: &'a str,
    pub(super) relation_removals: &'a [PackageRelationRemoval],
    pub(super) replacing_trove_id: Option<i64>,
    pub(super) config_declarations: &'a [SourceConfigDeclaration],
    pub(super) files: PayloadEffectFiles<'a>,
}

/// Compute one element's selected-root payload effects without mutating
/// anything.
///
/// This is the single authority behind both execution's `apply_payload` and the
/// pre-mutation interpreter projection.
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
    } = input;

    let resolved_files = files.resolve(selected_root, semantics)?;
    let directory_plan = inner::preflight_resolved_file_ownership(
        conn,
        selected_root,
        &resolved_files,
        package_name,
        relation_removals,
        semantics,
    )?;
    let all_package_files = files.live_root_files(&resolved_files)?;
    let config_candidates = all_package_files
        .iter()
        .filter(|file| !directory_plan.preserves_leaf(&file.path))
        .cloned()
        .collect::<Vec<_>>();
    let through_symlink_files = directory_plan.through_symlink_root_files(&resolved_files);
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
        &all_package_files,
        &mut config_plan.files,
    )?;

    Ok(ElementPayloadEffects {
        resolved_files,
        config_candidates,
        directory_plan,
        config_decisions: config_plan.decisions,
        install_files: config_plan.files,
        hardlink_references,
        through_symlink_files,
        remove_paths: config_plan.remove_paths,
    })
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
        },
    )
}

#[cfg(test)]
#[path = "payload_effects/tests.rs"]
mod tests;
