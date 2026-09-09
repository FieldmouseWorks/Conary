// apps/conary/src/commands/package_failure.rs
//! Project command-owned failures into the transport-neutral observation contract.

use super::install::native_events::NativePreflightContext;
use super::update::failure::UpdateFailures;
use conary_agent_contract::{
    NativePreflightCause, NativePreflightFailure, NativePreflightProgram, PackageFailure,
    PackageFailureReport, PackageFailureSchema,
};
use conary_core::ccs::native_transaction::{NativeEventProgram, NativeEventStage};
use conary_core::scriptlet::NativeLifecyclePreflightError;

#[derive(Debug, thiserror::Error)]
#[error("{summary}")]
pub(crate) struct PackageFailureScope {
    summary: String,
    causes: Vec<String>,
    pub root: String,
    pub database: String,
}

pub(crate) fn with_scope(error: anyhow::Error, root: &str, database: &str) -> anyhow::Error {
    if error.is::<NativePreflightContext>() {
        let summary = plain_failure_summary(&error);
        let causes = causes(&error);
        error.context(PackageFailureScope {
            summary,
            causes,
            root: root.into(),
            database: database.into(),
        })
    } else {
        error
    }
}

/// Scope is additional typed context, not a replacement for the failure's
/// ordinary Display contract. Avoid expanding an already retained chain twice.
pub(crate) fn plain_failure_summary(error: &anyhow::Error) -> String {
    error
        .downcast_ref::<PackageFailureScope>()
        .map_or_else(|| format!("{error:#}"), |scope| scope.summary.clone())
}

/// Return package failures without interpreting free-form text or querying mutable state.
/// Unknown command failures return None and retain the caller's existing error path.
pub fn package_failure_report(error: &anyhow::Error) -> Option<PackageFailureReport> {
    if let Some(updates) = error.downcast_ref::<UpdateFailures>() {
        return Some(PackageFailureReport {
            schema: PackageFailureSchema::V1,
            requested_updates: Some(updates.total_requested),
            committed_changesets: Some(updates.committed_changesets.clone()),
            failures: updates
                .failures
                .iter()
                .map(|failure| PackageFailure {
                    package: failure.package.clone(),
                    version: failure.version.clone(),
                    native_preflight: native_failure(&failure.error),
                    causes: if failure.error.is::<NativePreflightContext>() {
                        vec![]
                    } else {
                        causes(&failure.error)
                    },
                })
                .collect(),
        });
    }
    let native = native_failure(error)?;
    Some(PackageFailureReport {
        schema: PackageFailureSchema::V1,
        requested_updates: None,
        committed_changesets: None,
        failures: vec![PackageFailure {
            package: native.package.clone(),
            version: native.version.clone(),
            native_preflight: Some(native),
            causes: vec![],
        }],
    })
}

fn causes(error: &anyhow::Error) -> Vec<String> {
    // Scope's Display retains this chain for plain consumers. It is not a new
    // cause and must not repeat the entire chain in structured reports.
    error.downcast_ref::<PackageFailureScope>().map_or_else(
        || error.chain().map(ToString::to_string).collect(),
        |scope| scope.causes.clone(),
    )
}

fn native_failure(error: &anyhow::Error) -> Option<NativePreflightFailure> {
    let context = error.downcast_ref::<NativePreflightContext>()?;
    let (cause, notes) = match error.downcast_ref::<NativeLifecyclePreflightError>() {
        Some(NativeLifecyclePreflightError::MissingInterpreter { interpreter, entry_id, projected }) => (
            NativePreflightCause::MissingInterpreter { interpreter: interpreter.clone(), entry_id: entry_id.clone(), projected: *projected },
            vec!["Provide the required interpreter in the selected root at this lifecycle stage before retrying.".into()],
        ),
        Some(NativeLifecyclePreflightError::InvalidExecutionRoot { root }) => (
            NativePreflightCause::InvalidExecutionRoot { root: root.display().to_string() },
            vec!["Use an absolute materialized selected root other than '/'.".into()],
        ),
        Some(NativeLifecyclePreflightError::TimeoutOutOfRange { entry_id, timeout_ms, minimum_ms, maximum_ms }) => (
            NativePreflightCause::TimeoutOutOfRange { entry_id: entry_id.clone(), timeout_ms: *timeout_ms, minimum_ms: *minimum_ms, maximum_ms: *maximum_ms },
            vec!["Obtain a package with lifecycle timeout metadata inside the reported range.".into()],
        ),
        None => (NativePreflightCause::Unclassified { causes: causes(error) }, vec![]),
    };
    Some(NativePreflightFailure {
        package: context.package.clone(),
        version: context.version.clone(),
        architecture: context.architecture.clone(),
        source_format: context.source_format.clone(),
        execution_root: context.root.display().to_string(),
        requested_root: error
            .downcast_ref::<PackageFailureScope>()
            .map(|scope| scope.root.clone()),
        database: error
            .downcast_ref::<PackageFailureScope>()
            .map(|scope| scope.database.clone()),
        stage: stage_name(context.stage).into(),
        recovery: context.recovery,
        program: match &context.program {
            NativeEventProgram::BundleEntry { entry_id } => NativePreflightProgram::BundleEntry {
                entry_id: entry_id.clone(),
            },
            NativeEventProgram::Command { argv } => {
                NativePreflightProgram::Command { argv: argv.clone() }
            }
            NativeEventProgram::RpmSysusers { source_path } => {
                NativePreflightProgram::RpmSysusers {
                    source_path: source_path.clone(),
                }
            }
        },
        cause,
        notes,
    })
}

fn stage_name(stage: NativeEventStage) -> &'static str {
    match stage {
        NativeEventStage::ArchPreTransaction => "arch-pre-transaction",
        NativeEventStage::RpmPreTransaction => "rpm-pre-transaction",
        NativeEventStage::RpmPreUnTransaction => "rpm-pre-un-transaction",
        NativeEventStage::RpmTransactionFileTriggerUninstall => {
            "rpm-transaction-file-trigger-uninstall"
        }
        NativeEventStage::DebPreRemoveUpgrade => "deb-pre-remove-upgrade",
        NativeEventStage::DebPreDeconfigure => "deb-pre-deconfigure",
        NativeEventStage::DebPreRemoveInFavour => "deb-pre-remove-in-favour",
        NativeEventStage::RpmSysusers => "rpm-sysusers",
        NativeEventStage::RpmTriggerPreInstall => "rpm-trigger-pre-install",
        NativeEventStage::DebPreConfigure => "deb-pre-configure",
        NativeEventStage::PackagePreInstall => "package-pre-install",
        NativeEventStage::RpmFileTriggerInstallHigh => "rpm-file-trigger-install-high",
        NativeEventStage::DebPostRemoveUpgrade => "deb-post-remove-upgrade",
        NativeEventStage::DebPostInstall => "deb-post-install",
        NativeEventStage::DebAwaitedTriggerProcessing => "deb-awaited-trigger-processing",
        NativeEventStage::DebErrorRecovery => "deb-error-recovery",
        NativeEventStage::PackagePostInstall => "package-post-install",
        NativeEventStage::RpmTriggerInstall => "rpm-trigger-install",
        NativeEventStage::RpmFileTriggerInstallLow => "rpm-file-trigger-install-low",
        NativeEventStage::RpmFileTriggerUninstallHigh => "rpm-file-trigger-uninstall-high",
        NativeEventStage::RpmTriggerUninstall => "rpm-trigger-uninstall",
        NativeEventStage::PackagePreRemove => "package-pre-remove",
        NativeEventStage::RpmFileTriggerUninstallLow => "rpm-file-trigger-uninstall-low",
        NativeEventStage::RpmFileTriggerPostUninstallHigh => "rpm-file-trigger-post-uninstall-high",
        NativeEventStage::PackagePostRemove => "package-post-remove",
        NativeEventStage::DebPostRemovePurge => "deb-post-remove-purge",
        NativeEventStage::DebPostRemoveDisappear => "deb-post-remove-disappear",
        NativeEventStage::RpmTriggerPostUninstall => "rpm-trigger-post-uninstall",
        NativeEventStage::RpmFileTriggerPostUninstallLow => "rpm-file-trigger-post-uninstall-low",
        NativeEventStage::RpmPostTransaction => "rpm-post-transaction",
        NativeEventStage::RpmPostUnTransaction => "rpm-post-un-transaction",
        NativeEventStage::RpmDatabaseTransactionFileTriggerInstall => {
            "rpm-database-transaction-file-trigger-install"
        }
        NativeEventStage::RpmTransactionFileTriggerPostUninstall => {
            "rpm-transaction-file-trigger-post-uninstall"
        }
        NativeEventStage::RpmTransactionFileTriggerInstall => {
            "rpm-transaction-file-trigger-install"
        }
        NativeEventStage::DebTriggerProcessing => "deb-trigger-processing",
        NativeEventStage::ArchPostTransaction => "arch-post-transaction",
        NativeEventStage::EopkgSystemConfiguration => "eopkg-system-configuration",
    }
}

#[cfg(test)]
pub(crate) mod tests;
