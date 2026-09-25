// apps/conary/src/commands/install/native_events/preflight.rs

//! Transaction-wide runtime validation for exact native lifecycle programs.

use super::{NativePreflightContext, PreparedNativeTransaction, executor_for_owner, runtime};
use anyhow::{Context, Result};
use conary_core::ccs::native_transaction::{
    DebRecoveryResult, NativeEventPathProjection, NativeEventProgram, NativePackageIdentity,
    NativeTransactionChange, NativeTransactionEvent, NativeTransactionPlan,
};
use conary_core::filesystem::{ProjectedNode, SelectedRootProjection};
use conary_core::scriptlet::{
    ExecutionMode, NativeInterpreterResolution, SandboxMode, ScriptletExecutor,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Debug, Default)]
pub(super) struct NativePathProjection {
    events: Vec<NativeEventPathProjection>,
}

impl NativePathProjection {
    #[cfg(test)]
    pub(super) fn from_events(events: Vec<NativeEventPathProjection>) -> Self {
        Self { events }
    }

    pub(super) fn from_transaction(
        plan: &NativeTransactionPlan,
        changes: &[NativeTransactionChange],
        new_path_nodes: &[BTreeMap<String, ProjectedNode>],
        final_owned: &BTreeSet<String>,
    ) -> Result<Self> {
        let events = plan
            .graph
            .path_projections(plan.events.len(), changes, new_path_nodes, final_owned)?
            .into_iter()
            .enumerate()
            .map(|(event_index, projection)| {
                projection.with_context(|| {
                    format!(
                        "native transaction graph omitted path projection for event {event_index}"
                    )
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self { events })
    }

    fn for_event(&self, event_index: usize) -> Result<&NativeEventPathProjection> {
        self.events.get(event_index).with_context(|| {
            format!("native transaction preflight has no path projection for event {event_index}")
        })
    }

    fn interpreter_availability(
        projection: &NativeEventPathProjection,
        root: &Path,
        interpreter: &str,
    ) -> Result<NativeInterpreterResolution> {
        let mut selected = SelectedRootProjection::new(root);
        let projected = matches!(projection, NativeEventPathProjection::Projected { .. });
        if let NativeEventPathProjection::Projected {
            introduced,
            explicitly_removed,
        } = projection
        {
            for (path, node) in introduced {
                selected.insert(path, node.clone())?;
            }
            for path in explicitly_removed {
                selected.remove(path)?;
            }
        }
        let executable = selected
            .resolve_executable(interpreter)
            .with_context(|| format!("native interpreter path '{interpreter}' is invalid"))?;
        Ok(NativeInterpreterResolution {
            executable,
            projected,
        })
    }
}

impl PreparedNativeTransaction {
    /// Validate every normal and recovery event before the transaction runs
    /// lifecycle code or mutates package payload/database state.
    pub(crate) fn preflight(&self, root: &Path, mode: &ExecutionMode) -> Result<()> {
        for (event_index, event) in self.plan.events.iter().enumerate() {
            let projection = self.path_projection.for_event(event_index)?;
            self.preflight_event(event, root, mode, projection)
                .context(NativePreflightContext::event(event, root, false))?;
            if let Some(recovery) = self.plan.deb.recovery_for_event(event) {
                self.preflight_deb_recovery(recovery, root, mode, projection)?;
            }
        }

        let current_root = NativeEventPathProjection::CurrentRoot;
        let mut payload_recovery_owners = BTreeSet::new();
        for owner in &self.owners {
            let package = NativePackageIdentity {
                package_name: owner.package_name.clone(),
                package_version: owner.package_version.clone(),
                package_arch: owner.bundle.source_arch.clone(),
            };
            if payload_recovery_owners.insert(package.clone())
                && let Some(recovery) = self.plan.deb.recovery_for_payload_failure(&package)
            {
                // A payload failure can occur before the incoming interpreter
                // has been exposed. Only the pre-transaction root is guaranteed.
                self.preflight_deb_recovery(recovery, root, mode, &current_root)?;
            }
        }

        self.preflight_arch_ldconfig(root)
    }

    fn preflight_event(
        &self,
        event: &NativeTransactionEvent,
        root: &Path,
        mode: &ExecutionMode,
        projection: &NativeEventPathProjection,
    ) -> Result<()> {
        let owner = self.owner_for_event(event)?;
        let executor = executor_for_owner(owner, root)?;
        match &event.program {
            NativeEventProgram::BundleEntry { entry_id } => {
                let entry = runtime::bundle_entry_for_event(owner, entry_id)?;
                let interpreter_availability = NativePathProjection::interpreter_availability(
                    projection,
                    root,
                    &entry.interpreter,
                )?;
                runtime::preflight_entry(
                    owner,
                    &executor,
                    runtime::EntryInvocation::new(
                        entry_id,
                        mode,
                        &event.args,
                        &event.stdin,
                        event.deb_package_refcount,
                    ),
                    interpreter_availability,
                )
            }
            NativeEventProgram::Command { argv } => executor
                .preflight_native_command(argv)
                .map_err(anyhow::Error::from),
            NativeEventProgram::RpmSysusers { source_path } => executor
                .preflight_rpm_sysusers(self.rpm_sysusers_interface()?, source_path.as_deref())
                .map_err(anyhow::Error::from),
        }
    }

    fn preflight_deb_recovery(
        &self,
        recovery: &DebRecoveryResult,
        root: &Path,
        mode: &ExecutionMode,
        projection: &NativeEventPathProjection,
    ) -> Result<()> {
        match recovery {
            DebRecoveryResult::Disposition(_) => Ok(()),
            DebRecoveryResult::PersistState(transition) => {
                self.preflight_deb_recovery(&transition.next, root, mode, projection)
            }
            DebRecoveryResult::Run(node) => {
                self.preflight_event(&node.event, root, mode, projection)
                    .context(NativePreflightContext::event(&node.event, root, true))?;
                self.preflight_deb_recovery(&node.on_success, root, mode, projection)?;
                self.preflight_deb_recovery(&node.on_failure, root, mode, projection)
            }
        }
    }

    fn preflight_arch_ldconfig(&self, root: &Path) -> Result<()> {
        let Some(argv) = self.arch_ldconfig_argv(root)? else {
            return Ok(());
        };
        ScriptletExecutor::new(
            root,
            "libalpm-transaction",
            "1",
            conary_core::scriptlet::PackageFormat::Arch,
        )
        .with_sandbox_mode(SandboxMode::Always)
        .preflight_native_command(&argv)
        .map_err(anyhow::Error::from)
    }
}
