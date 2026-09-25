// crates/conary-core/src/ccs/native_transaction/graph.rs

//! Typed payload boundaries for native package-manager transaction execution.

use super::{
    NativeEventPlacement, NativeEventStage, NativeTransactionChange, NativeTransactionEvent,
};
use crate::filesystem::ProjectedNode;
use anyhow::{Result, bail};
use std::collections::{BTreeMap, BTreeSet};

/// One executable node in a native transaction.
///
/// Event nodes index `NativeTransactionPlan::events`. Payload nodes index the
/// original `NativeTransactionChange` slice supplied to the planner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebTriggerActivationBoundary {
    BeforePayloadEvents,
    BeforePayloadMutation,
    BeforeOldPayloadFinalization,
    BeforeConfigure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeTransactionStep {
    RunEvent {
        event_index: usize,
    },
    PersistDebTriggerActivations {
        change_index: usize,
        transaction_index: usize,
        boundary: DebTriggerActivationBoundary,
    },
    ApplyPayload {
        change_index: usize,
    },
    FinalizeOldPayload {
        change_index: usize,
    },
    /// Remove the Debian conffile set after ordinary payload removal and
    /// `postrm remove`, but before `postrm purge`.
    PurgeConfigFiles {
        change_index: usize,
    },
}

/// Package paths visible to one event after replaying preceding graph nodes.
///
/// The projection is a typed overlay: the nodes introduced before the event and
/// the paths tombstoned before it. It is consumed by the selected-root resolver
/// together with the on-disk selected root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeEventPathProjection {
    CurrentRoot,
    Projected {
        introduced: BTreeMap<String, ProjectedNode>,
        explicitly_removed: BTreeSet<String>,
    },
}

/// Exact transaction execution order, including filesystem visibility changes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NativeTransactionGraph {
    pub steps: Vec<NativeTransactionStep>,
}

impl NativeTransactionGraph {
    /// Replay exact payload boundaries into one typed overlay per event.
    ///
    /// Paths are returned in normalized archive form without a leading slash.
    /// `new_path_nodes` carries the typed node for each change's introduced
    /// paths; a path without a node is resolved against the on-disk selected
    /// root instead of overlaid. An ownership transfer only preserves a path
    /// when the new owner has already crossed its `ApplyPayload` boundary.
    pub fn path_projections(
        &self,
        events_len: usize,
        changes: &[NativeTransactionChange],
        new_path_nodes: &[BTreeMap<String, ProjectedNode>],
        final_owned_paths: &BTreeSet<String>,
    ) -> Result<Vec<Option<NativeEventPathProjection>>> {
        if new_path_nodes.len() != changes.len() {
            bail!(
                "native transaction node projections have length {}, expected {}",
                new_path_nodes.len(),
                changes.len()
            );
        }
        let final_owned_paths = normalize_paths(final_owned_paths)?;
        let mut projections = vec![None; events_len];
        let mut introduced_paths = BTreeSet::new();
        let mut introduced_nodes = BTreeMap::<String, ProjectedNode>::new();
        let mut explicitly_removed_paths = BTreeSet::new();
        let mut applied_new_owners = BTreeMap::<String, BTreeSet<usize>>::new();
        let mut payload_boundary_crossed = false;
        let mut finalized_changes = 0usize;
        let payload_change_count = changes
            .iter()
            .filter(|change| {
                !matches!(
                    change.operation,
                    super::NativeTransactionOperation::DeconfigureInFavour { .. }
                )
            })
            .count();

        for step in &self.steps {
            match *step {
                NativeTransactionStep::RunEvent { event_index } => {
                    let projection = projections.get_mut(event_index).ok_or_else(|| {
                        anyhow::anyhow!(
                            "native transaction graph refers to missing event {event_index}"
                        )
                    })?;
                    if projection.is_some() {
                        bail!("native transaction graph repeats event {event_index}");
                    }
                    *projection = Some(if payload_boundary_crossed {
                        NativeEventPathProjection::Projected {
                            introduced: introduced_nodes.clone(),
                            explicitly_removed: explicitly_removed_paths.clone(),
                        }
                    } else {
                        NativeEventPathProjection::CurrentRoot
                    });
                }
                NativeTransactionStep::PersistDebTriggerActivations {
                    change_index,
                    transaction_index,
                    ..
                } => {
                    let change = changes.get(change_index).ok_or_else(|| {
                        anyhow::anyhow!(
                            "native transaction graph refers to missing trigger-activation change {change_index}"
                        )
                    })?;
                    if change.transaction_index != transaction_index {
                        bail!(
                            "native transaction graph trigger-activation step for change {change_index} carries source index {transaction_index}, expected {}",
                            change.transaction_index
                        );
                    }
                }
                NativeTransactionStep::ApplyPayload { change_index } => {
                    let change = changes.get(change_index).ok_or_else(|| {
                        anyhow::anyhow!(
                            "native transaction graph refers to missing payload change {change_index}"
                        )
                    })?;
                    let new_paths = normalize_paths(&change.new_paths)?;
                    let nodes = normalize_node_map(&new_path_nodes[change_index])?;
                    apply_visible_paths(
                        new_paths.clone(),
                        change_index,
                        &mut introduced_paths,
                        &mut explicitly_removed_paths,
                        &mut applied_new_owners,
                    );
                    for path in new_paths {
                        if let Some(node) = nodes.get(&path) {
                            introduced_nodes.insert(path, node.clone());
                        }
                    }
                    payload_boundary_crossed = true;
                }
                NativeTransactionStep::FinalizeOldPayload { change_index } => {
                    let change = changes.get(change_index).ok_or_else(|| {
                        anyhow::anyhow!(
                            "native transaction graph refers to missing old-payload change {change_index}"
                        )
                    })?;
                    finalize_visible_paths(
                        normalize_paths(&change.old_paths)?,
                        normalize_paths(&change.new_paths)?,
                        &mut introduced_paths,
                        &mut introduced_nodes,
                        &mut explicitly_removed_paths,
                        &applied_new_owners,
                    );
                    finalized_changes += 1;
                    if finalized_changes == payload_change_count {
                        introduced_paths.retain(|path| final_owned_paths.contains(path));
                        introduced_nodes.retain(|path, _| final_owned_paths.contains(path));
                        explicitly_removed_paths.retain(|path| !final_owned_paths.contains(path));
                    }
                    payload_boundary_crossed = true;
                }
                NativeTransactionStep::PurgeConfigFiles { change_index } => {
                    let change = changes.get(change_index).ok_or_else(|| {
                        anyhow::anyhow!(
                            "native transaction graph refers to missing config-purge change {change_index}"
                        )
                    })?;
                    if change.operation != super::NativeTransactionOperation::Purge {
                        bail!(
                            "native transaction graph purges config for non-purge change {change_index}"
                        );
                    }
                    payload_boundary_crossed = true;
                }
            }
        }

        if projections.iter().any(Option::is_none) {
            bail!("native transaction graph does not place every planned event");
        }
        Ok(projections)
    }
}

fn apply_visible_paths(
    paths: BTreeSet<String>,
    change_index: usize,
    introduced: &mut BTreeSet<String>,
    explicitly_removed: &mut BTreeSet<String>,
    applied_new_owners: &mut BTreeMap<String, BTreeSet<usize>>,
) {
    for path in paths {
        applied_new_owners
            .entry(path.clone())
            .or_default()
            .insert(change_index);
        explicitly_removed.remove(&path);
        introduced.insert(path);
    }
}

fn finalize_visible_paths(
    old_paths: BTreeSet<String>,
    new_paths: BTreeSet<String>,
    introduced: &mut BTreeSet<String>,
    introduced_nodes: &mut BTreeMap<String, ProjectedNode>,
    explicitly_removed: &mut BTreeSet<String>,
    applied_new_owners: &BTreeMap<String, BTreeSet<usize>>,
) {
    for path in old_paths.difference(&new_paths) {
        if applied_new_owners
            .get(path)
            .is_some_and(|owners| !owners.is_empty())
        {
            continue;
        }
        introduced.remove(path);
        introduced_nodes.remove(path);
        explicitly_removed.insert(path.clone());
    }
}

fn normalize_node_map(
    nodes: &BTreeMap<String, ProjectedNode>,
) -> Result<BTreeMap<String, ProjectedNode>> {
    nodes
        .iter()
        .map(|(path, node)| Ok((normalize_path(path)?, node.clone())))
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ElementBoundary {
    BeforePayload,
    BeforeOldPayloadFinalization,
    AfterOldPayloadFinalization,
    AfterConfigPurge,
}

pub(super) fn build(
    events: &mut [NativeTransactionEvent],
    changes: &[NativeTransactionChange],
) -> Result<NativeTransactionGraph> {
    let change_indices_by_transaction_index = changes
        .iter()
        .enumerate()
        .map(|(change_index, change)| (change.transaction_index, change_index))
        .collect::<BTreeMap<_, _>>();

    for event in events.iter() {
        validate_placement(event, &change_indices_by_transaction_index)?;
    }
    events.sort_by(|left, right| {
        placement_order(left.placement)
            .cmp(&placement_order(right.placement))
            .then_with(|| left.stage.cmp(&right.stage))
            .then_with(|| left.order_key.cmp(&right.order_key))
            .then_with(|| left.owner_package.cmp(&right.owner_package))
            .then_with(|| left.owner_arch.cmp(&right.owner_arch))
    });

    let mut steps = Vec::with_capacity(events.len() + changes.len() * 6);
    append_transaction_events(&mut steps, events, NativeEventPlacement::TransactionBefore);

    let mut ordered_changes = changes.iter().enumerate().collect::<Vec<_>>();
    ordered_changes.sort_by_key(|(_, change)| change.transaction_index);
    for (change_index, change) in ordered_changes {
        if matches!(
            change.operation,
            super::NativeTransactionOperation::DeconfigureInFavour { .. }
        ) {
            continue;
        }
        steps.push(NativeTransactionStep::PersistDebTriggerActivations {
            change_index,
            transaction_index: change.transaction_index,
            boundary: DebTriggerActivationBoundary::BeforePayloadEvents,
        });
        append_element_events(
            &mut steps,
            events,
            change.transaction_index,
            ElementBoundary::BeforePayload,
        );
        steps.push(NativeTransactionStep::PersistDebTriggerActivations {
            change_index,
            transaction_index: change.transaction_index,
            boundary: DebTriggerActivationBoundary::BeforePayloadMutation,
        });
        steps.push(NativeTransactionStep::ApplyPayload { change_index });
        append_element_events(
            &mut steps,
            events,
            change.transaction_index,
            ElementBoundary::BeforeOldPayloadFinalization,
        );
        steps.push(NativeTransactionStep::PersistDebTriggerActivations {
            change_index,
            transaction_index: change.transaction_index,
            boundary: DebTriggerActivationBoundary::BeforeOldPayloadFinalization,
        });
        steps.push(NativeTransactionStep::FinalizeOldPayload { change_index });
        steps.push(NativeTransactionStep::PersistDebTriggerActivations {
            change_index,
            transaction_index: change.transaction_index,
            boundary: DebTriggerActivationBoundary::BeforeConfigure,
        });
        append_element_events(
            &mut steps,
            events,
            change.transaction_index,
            ElementBoundary::AfterOldPayloadFinalization,
        );
        if change.operation == super::NativeTransactionOperation::Purge {
            steps.push(NativeTransactionStep::PurgeConfigFiles { change_index });
            append_element_events(
                &mut steps,
                events,
                change.transaction_index,
                ElementBoundary::AfterConfigPurge,
            );
        }
    }

    append_transaction_events(&mut steps, events, NativeEventPlacement::TransactionAfter);
    Ok(NativeTransactionGraph { steps })
}

fn append_transaction_events(
    steps: &mut Vec<NativeTransactionStep>,
    events: &[NativeTransactionEvent],
    placement: NativeEventPlacement,
) {
    steps.extend(
        events
            .iter()
            .enumerate()
            .filter(|(_, event)| event.placement == placement)
            .map(|(event_index, _)| NativeTransactionStep::RunEvent { event_index }),
    );
}

fn append_element_events(
    steps: &mut Vec<NativeTransactionStep>,
    events: &[NativeTransactionEvent],
    transaction_index: usize,
    boundary: ElementBoundary,
) {
    steps.extend(
        events
            .iter()
            .enumerate()
            .filter_map(|(event_index, event)| {
                if event.placement
                    != (NativeEventPlacement::TransactionElement { transaction_index })
                    || element_boundary(event.stage) != Some(boundary)
                {
                    return None;
                }
                Some(NativeTransactionStep::RunEvent { event_index })
            }),
    );
}

fn validate_placement(
    event: &NativeTransactionEvent,
    change_indices_by_transaction_index: &BTreeMap<usize, usize>,
) -> Result<()> {
    match event.placement {
        NativeEventPlacement::TransactionBefore if transaction_before_stage(event.stage) => Ok(()),
        NativeEventPlacement::TransactionAfter if transaction_after_stage(event.stage) => Ok(()),
        NativeEventPlacement::TransactionElement { transaction_index }
            if change_indices_by_transaction_index.contains_key(&transaction_index)
                && element_boundary(event.stage).is_some() =>
        {
            Ok(())
        }
        NativeEventPlacement::TransactionElement { transaction_index }
            if !change_indices_by_transaction_index.contains_key(&transaction_index) =>
        {
            bail!(
                "native event '{}' refers to missing transaction element {}",
                event.order_key,
                transaction_index
            )
        }
        _ => bail!(
            "native event '{}' places stage {:?} at an invalid transaction boundary",
            event.order_key,
            event.stage
        ),
    }
}

fn placement_order(placement: NativeEventPlacement) -> (u8, usize) {
    match placement {
        NativeEventPlacement::TransactionBefore => (0, 0),
        NativeEventPlacement::TransactionElement { transaction_index } => (1, transaction_index),
        NativeEventPlacement::TransactionAfter => (2, 0),
    }
}

fn transaction_before_stage(stage: NativeEventStage) -> bool {
    matches!(
        stage,
        NativeEventStage::ArchPreTransaction
            | NativeEventStage::RpmPreTransaction
            | NativeEventStage::RpmPreUnTransaction
            | NativeEventStage::RpmTransactionFileTriggerUninstall
    )
}

fn transaction_after_stage(stage: NativeEventStage) -> bool {
    matches!(
        stage,
        NativeEventStage::RpmPostTransaction
            | NativeEventStage::RpmPostUnTransaction
            | NativeEventStage::RpmDatabaseTransactionFileTriggerInstall
            | NativeEventStage::RpmTransactionFileTriggerPostUninstall
            | NativeEventStage::RpmTransactionFileTriggerInstall
            | NativeEventStage::DebAwaitedTriggerProcessing
            | NativeEventStage::DebTriggerProcessing
            | NativeEventStage::ArchPostTransaction
            | NativeEventStage::EopkgSystemConfiguration
    )
}

fn element_boundary(stage: NativeEventStage) -> Option<ElementBoundary> {
    match stage {
        NativeEventStage::DebPreRemoveUpgrade
        | NativeEventStage::DebPreDeconfigure
        | NativeEventStage::DebPreRemoveInFavour
        | NativeEventStage::RpmSysusers
        | NativeEventStage::RpmTriggerPreInstall
        | NativeEventStage::DebPreConfigure
        | NativeEventStage::PackagePreInstall => Some(ElementBoundary::BeforePayload),
        NativeEventStage::RpmFileTriggerInstallHigh
        | NativeEventStage::DebPostRemoveUpgrade
        | NativeEventStage::PackagePostInstall
        | NativeEventStage::RpmTriggerInstall
        | NativeEventStage::RpmFileTriggerInstallLow
        | NativeEventStage::RpmFileTriggerUninstallHigh
        | NativeEventStage::RpmTriggerUninstall
        | NativeEventStage::PackagePreRemove
        | NativeEventStage::RpmFileTriggerUninstallLow => {
            Some(ElementBoundary::BeforeOldPayloadFinalization)
        }
        NativeEventStage::RpmFileTriggerPostUninstallHigh
        | NativeEventStage::DebPostInstall
        | NativeEventStage::PackagePostRemove
        | NativeEventStage::RpmTriggerPostUninstall
        | NativeEventStage::RpmFileTriggerPostUninstallLow => {
            Some(ElementBoundary::AfterOldPayloadFinalization)
        }
        NativeEventStage::DebPostRemovePurge => Some(ElementBoundary::AfterConfigPurge),
        NativeEventStage::DebPostRemoveDisappear => {
            Some(ElementBoundary::BeforeOldPayloadFinalization)
        }
        NativeEventStage::ArchPreTransaction
        | NativeEventStage::RpmPreTransaction
        | NativeEventStage::RpmPreUnTransaction
        | NativeEventStage::RpmTransactionFileTriggerUninstall
        | NativeEventStage::DebAwaitedTriggerProcessing
        | NativeEventStage::DebErrorRecovery
        | NativeEventStage::RpmPostTransaction
        | NativeEventStage::RpmPostUnTransaction
        | NativeEventStage::RpmDatabaseTransactionFileTriggerInstall
        | NativeEventStage::RpmTransactionFileTriggerPostUninstall
        | NativeEventStage::RpmTransactionFileTriggerInstall
        | NativeEventStage::DebTriggerProcessing
        | NativeEventStage::ArchPostTransaction
        | NativeEventStage::EopkgSystemConfiguration => None,
    }
}

fn normalize_paths(paths: &BTreeSet<String>) -> Result<BTreeSet<String>> {
    paths
        .iter()
        .map(|path| normalize_path(path))
        .collect::<Result<_>>()
}

fn normalize_path(path: &str) -> Result<String> {
    if path.contains('\0') {
        bail!("native transaction path contains NUL");
    }
    let mut components = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => bail!("native transaction path '{path}' escapes its archive root"),
            component => components.push(component),
        }
    }
    if components.is_empty() {
        bail!("native transaction path '{path}' does not name a file");
    }
    Ok(components.join("/"))
}

#[cfg(test)]
mod tests;
