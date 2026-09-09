// apps/conary/src/commands/install/native_events.rs

//! Install-side execution of exact native package-manager transaction events.
//! Package identity construction is owned by `native_events/identity.rs`.

use anyhow::{Context, Result};
use conary_core::ccs::native_lifecycle::NativeLifecycleBundle;
use conary_core::ccs::native_transaction::{
    DebPackageState, NativeBundleRole, NativeBundleView, NativeInstalledCapability,
    NativePackageIdentity, NativeTransactionChange, NativeTransactionOperation,
    NativeTransactionPathCapabilities, NativeTransactionPlan, NativeTransactionState,
    plan_native_transaction,
};
use conary_core::db::models::{
    ConfigFile, InstalledNativeLifecycleBundle, NativeLifecycleResidualState,
    PackagePayloadOwnership, Trove,
};
use conary_core::repository::dependency_model::PackageRelationRemovalMode;
use conary_core::repository::versioning::VersionScheme;
use conary_core::scriptlet::{SandboxMode, ScriptletExecutor};
use conary_core::transaction::{
    PackageRelationDeconfiguration, PackageRelationDeconfigurationCause, PackageRelationRemoval,
    validate_package_relation_transitions,
};
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;
use std::path::Path;

#[path = "native_events/deb_state.rs"]
mod deb_state;
#[path = "native_events/debian_runtime.rs"]
mod debian_runtime;
#[path = "native_events/execution.rs"]
mod execution;
#[path = "native_events/graph_execution.rs"]
mod graph_execution;
mod identity;
mod refusal;
pub(crate) use refusal::NativePreflightContext;
mod install;
#[path = "native_events/preflight.rs"]
mod preflight;
#[path = "native_events/runtime.rs"]
mod runtime;
#[path = "native_events/transaction_state.rs"]
mod transaction_state;

pub(super) use identity::deb_identity_for_trove;
use identity::owner_identity;
use preflight::NativePathProjection;
use transaction_state::{
    declared_capabilities_excluding, declared_path_capabilities_for_trove,
    installed_instance_count, installed_packages_before, path_capabilities,
    sort_and_deduplicate_capabilities,
};

pub(super) struct NativeInstallInput<'a> {
    pub package_name: &'a str,
    pub package_version: &'a str,
    pub package_arch: Option<&'a str>,
    pub version_scheme: VersionScheme,
    pub provides: &'a [conary_core::repository::dependency_model::ProvidedCapability],
    pub new_bundle: Option<&'a NativeLifecycleBundle>,
    pub old_trove: Option<&'a Trove>,
    pub relation_removals: &'a [PackageRelationRemoval],
    pub relation_deconfigurations: &'a [PackageRelationDeconfiguration],
    pub paths: Vec<String>,
}

pub(crate) struct NativeRemoveInput<'a> {
    pub trove: &'a Trove,
    pub paths: Vec<String>,
    pub operation: NativeTransactionOperation,
}

#[derive(Debug)]
struct NativeBundleOwner {
    package_name: String,
    package_version: String,
    instances_after: u32,
    role: NativeBundleRole,
    initial_package_state: DebPackageState,
    initial_pending_triggers: Vec<String>,
    initial_awaited_packages: Vec<NativePackageIdentity>,
    bundle: NativeLifecycleBundle,
}

#[derive(Debug)]
pub(crate) struct DebLifecycleFailure {
    pub(crate) package_name: String,
    pub(crate) package_state: DebPackageState,
    primary_error: String,
}

impl fmt::Display for DebLifecycleFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Debian lifecycle for '{}' stopped in state '{}': {}",
            self.package_name,
            self.package_state.as_str(),
            self.primary_error
        )
    }
}

impl std::error::Error for DebLifecycleFailure {}

#[derive(Debug, Default)]
pub(crate) struct PreparedNativeTransaction {
    owners: Vec<NativeBundleOwner>,
    plan: NativeTransactionPlan,
    changes: Vec<NativeTransactionChange>,
    transaction_state: NativeTransactionState,
    debian_config_before: Vec<debian_runtime::DebianConfigSnapshot>,
    operations: Vec<NativeTransactionOperation>,
    requires_upgrade_payload_boundary: bool,
    arch_transaction: bool,
    arch_ldconfig_required_after: bool,
    host_capabilities: Option<conary_core::ccs::HostCapabilityInventory>,
    path_projection: NativePathProjection,
    activation_invocations: RefCell<Vec<CapturedNativeActivation>>,
    continued_lifecycle_failures: RefCell<Vec<ContinuedLifecycleFailure>>,
}

/// A lifecycle entry that failed in a warn-and-continue class.
///
/// The source format proceeds past this failure, so the transaction does too,
/// but the failure stays typed evidence rather than becoming a silent success.
#[derive(Debug, Clone)]
pub(crate) struct ContinuedLifecycleFailure {
    pub(crate) package: String,
    pub(crate) version: String,
    pub(crate) entry: String,
    pub(crate) failure: conary_core::scriptlet::ScriptletFailureOutcome,
}

#[derive(Debug, Clone)]
pub(crate) struct CapturedNativeActivation {
    pub(crate) source_package: String,
    pub(crate) source_version: String,
    pub(crate) source_entry: String,
    pub(crate) invocation: conary_core::activation::RuntimeActivationInvocation,
}

impl PreparedNativeTransaction {
    pub(super) fn prepare_install(
        conn: &rusqlite::Connection,
        input: NativeInstallInput<'_>,
    ) -> Result<Self> {
        Self::prepare_batch(conn, &[input])
    }

    pub(super) fn prepare_batch(
        conn: &rusqlite::Connection,
        inputs: &[NativeInstallInput<'_>],
    ) -> Result<Self> {
        Self::prepare_batch_with_declared_paths(conn, inputs, &Default::default())
    }

    pub(crate) fn prepare_remove(
        conn: &rusqlite::Connection,
        trove_id: i64,
        package_name: &str,
        package_version: &str,
        paths: Vec<String>,
        purge_config_files: bool,
    ) -> Result<Self> {
        let removed_trove = Trove::find_by_id(conn, trove_id)?
            .with_context(|| format!("installed trove {trove_id} disappeared"))?;
        if removed_trove.name != package_name || removed_trove.version != package_version {
            anyhow::bail!(
                "installed trove {trove_id} identity changed during removal preparation: expected {package_name} {package_version}, found {} {}",
                removed_trove.name,
                removed_trove.version
            );
        }
        let version_scheme = removed_trove.version_scheme;
        let operation = if purge_config_files && version_scheme == VersionScheme::Debian {
            NativeTransactionOperation::Purge
        } else {
            NativeTransactionOperation::Remove
        };
        Self::prepare_removals(
            conn,
            &[NativeRemoveInput {
                trove: &removed_trove,
                paths,
                operation,
            }],
        )
    }

    pub(crate) fn prepare_removals(
        conn: &rusqlite::Connection,
        inputs: &[NativeRemoveInput<'_>],
    ) -> Result<Self> {
        if inputs.is_empty() {
            return Ok(Self::default());
        }

        let mut removed_trove_ids = HashSet::new();
        let mut counts_after = HashMap::<String, u32>::new();
        let mut changes = Vec::with_capacity(inputs.len());
        let mut path_capability_changes = Vec::with_capacity(inputs.len());
        let mut deb_change_indices = BTreeSet::new();
        let mut arch_transaction = false;
        for (transaction_index, input) in inputs.iter().enumerate() {
            let trove_id = input
                .trove
                .id
                .context("native removal input has no installed trove id")?;
            let current = Trove::find_by_id(conn, trove_id)?
                .with_context(|| format!("installed trove {trove_id} disappeared"))?;
            if current.name != input.trove.name
                || current.version != input.trove.version
                || current.architecture != input.trove.architecture
            {
                anyhow::bail!(
                    "installed trove {trove_id} identity changed during removal preparation"
                );
            }
            let version_scheme = current.version_scheme;
            if input.operation.counterpart_transaction_index().is_some() {
                anyhow::bail!(
                    "standalone native removal for '{}' cannot refer to an installing transaction element",
                    current.name
                );
            }
            if input.operation == NativeTransactionOperation::Purge
                && version_scheme != VersionScheme::Debian
            {
                anyhow::bail!(
                    "native purge operation is only valid for a Debian package, found '{}'",
                    current.name
                );
            }
            arch_transaction |= version_scheme == VersionScheme::Arch;
            if version_scheme == VersionScheme::Debian {
                deb_change_indices.insert(transaction_index);
            }
            if !removed_trove_ids.insert(trove_id) {
                anyhow::bail!("native removal input repeats installed trove {trove_id}");
            }
            let instances_before = counts_after
                .get(input.trove.name.as_str())
                .copied()
                .unwrap_or(installed_instance_count(conn, &input.trove.name)?);
            let instances_after = instances_before.checked_sub(1).with_context(|| {
                format!(
                    "removal transaction for '{}' has no installed instance",
                    input.trove.name
                )
            })?;
            counts_after.insert(input.trove.name.clone(), instances_after);
            let old_path_capabilities = declared_path_capabilities_for_trove(conn, trove_id)?;
            changes.push(NativeTransactionChange {
                package_name: input.trove.name.clone(),
                old_arch: InstalledNativeLifecycleBundle::find_by_trove(conn, trove_id)?
                    .and_then(|installed| installed.source_arch)
                    .or_else(|| input.trove.architecture.clone()),
                new_arch: None,
                old_version: Some(input.trove.version.clone()),
                new_version: None,
                operation: input.operation,
                old_paths: input.paths.iter().cloned().collect(),
                new_paths: BTreeSet::new(),
                instances_before,
                instances_after,
                transaction_index,
            });
            path_capability_changes.push(NativeTransactionPathCapabilities {
                old_paths: old_path_capabilities,
                new_paths: BTreeSet::new(),
            });
        }

        let mut owners = Vec::new();
        let installed_bundles = InstalledNativeLifecycleBundle::find_all(conn)?;
        for installed in installed_bundles {
            let bundle = installed.bundle().with_context(|| {
                format!(
                    "installed native lifecycle bundle for {} is malformed",
                    installed.source_package
                )
            })?;
            let role = if removed_trove_ids.contains(&installed.trove_id) {
                NativeBundleRole::Removing
            } else {
                NativeBundleRole::Installed
            };
            let instances_after = counts_after
                .get(installed.source_package.as_str())
                .copied()
                .unwrap_or(installed_instance_count(conn, &installed.source_package)?);
            owners.push(NativeBundleOwner {
                role,
                instances_after,
                package_name: installed.source_package,
                package_version: installed.source_version,
                initial_package_state: installed.lifecycle_state,
                initial_pending_triggers: installed.pending_triggers,
                initial_awaited_packages: installed.awaited_packages,
                bundle,
            });
        }

        if !native_global_state_has_possible_consumer(
            conn,
            &owners,
            arch_transaction,
            &deb_change_indices,
        )? && let Some(prepared) =
            Self::prepare_without_global_native_state(&changes, &path_capability_changes, false)?
        {
            return Ok(prepared);
        }

        let views = owners
            .iter()
            .map(|owner| NativeBundleView {
                package_name: &owner.package_name,
                package_version: &owner.package_version,
                instances_after: owner.instances_after,
                role: owner.role,
                bundle: &owner.bundle,
            })
            .collect::<Vec<_>>();
        let installed_capabilities_after =
            declared_capabilities_excluding(conn, &removed_trove_ids)?;
        let installed_path_capabilities_after = path_capabilities(
            installed_capabilities_after
                .iter()
                .map(|capability| capability.name.as_str()),
        );
        let installed_paths_after =
            PackagePayloadOwnership::installed_paths_excluding(conn, &removed_trove_ids)?;
        let arch_ldconfig_required_after =
            archive_paths_contain(&installed_paths_after, "etc/ld.so.conf");
        let transaction_state = NativeTransactionState {
            installed_packages_before: installed_packages_before(conn)?,
            installed_capabilities_after,
            installed_paths_after: installed_paths_after.clone(),
            deb_package_states: deb_state::transaction_package_states(conn, &owners)?,
            deb_change_indices,
        };
        let plan = plan_native_transaction(&views, &changes, &transaction_state)?;
        let host_capabilities = host_capabilities_for_plan(conn, arch_transaction, &plan)?;
        let path_projection = NativePathProjection::from_transaction(
            &plan,
            &changes,
            &installed_paths_after,
            &path_capability_changes,
            &installed_path_capabilities_after,
        )?;
        Ok(Self {
            owners,
            plan,
            changes: changes.clone(),
            transaction_state,
            debian_config_before: debian_runtime::config_snapshots(conn)?,
            operations: changes.iter().map(|change| change.operation).collect(),
            requires_upgrade_payload_boundary: false,
            arch_transaction,
            arch_ldconfig_required_after,
            host_capabilities,
            path_projection,
            activation_invocations: RefCell::new(Vec::new()),
            continued_lifecycle_failures: RefCell::new(Vec::new()),
        })
    }

    pub(crate) fn requires_upgrade_payload_boundary(&self) -> bool {
        self.requires_upgrade_payload_boundary
    }

    /// Build the exact payload graph without projecting unrelated installed
    /// native state. The lightweight plan is accepted only when it produces no
    /// lifecycle event; a future planner-owned event therefore falls back to
    /// the complete projection path rather than silently losing its inputs.
    fn prepare_without_global_native_state(
        changes: &[NativeTransactionChange],
        path_capability_changes: &[NativeTransactionPathCapabilities],
        requires_upgrade_payload_boundary: bool,
    ) -> Result<Option<Self>> {
        let transaction_state = NativeTransactionState::default();
        let plan = plan_native_transaction(&[], changes, &transaction_state)?;
        if !plan.events.is_empty() {
            return Ok(None);
        }
        let empty_paths = BTreeSet::new();
        let path_projection = NativePathProjection::from_transaction(
            &plan,
            changes,
            &empty_paths,
            path_capability_changes,
            &empty_paths,
        )?;
        Ok(Some(Self {
            owners: Vec::new(),
            plan,
            changes: changes.to_vec(),
            transaction_state,
            debian_config_before: Vec::new(),
            operations: changes.iter().map(|change| change.operation).collect(),
            requires_upgrade_payload_boundary,
            arch_transaction: false,
            arch_ldconfig_required_after: false,
            host_capabilities: None,
            path_projection,
            activation_invocations: RefCell::new(Vec::new()),
            continued_lifecycle_failures: RefCell::new(Vec::new()),
        }))
    }

    pub(super) fn rpm_sysusers_interface(&self) -> Result<&conary_core::ccs::ExecutableInterface> {
        self.host_capabilities
            .as_ref()
            .context("RPM sysusers event lost its required typed host capability inventory")?
            .sysusers_interface()
            .map_err(anyhow::Error::from)
    }
}

/// Whether any typed authority can consume the transaction-wide package,
/// capability, or path projections. Ambiguous or present authority takes the
/// complete path; only an exact absence of all consumers admits the compact
/// payload-graph path.
fn native_global_state_has_possible_consumer(
    conn: &rusqlite::Connection,
    owners: &[NativeBundleOwner],
    arch_transaction: bool,
    deb_change_indices: &BTreeSet<usize>,
) -> Result<bool> {
    if !owners.is_empty() || arch_transaction || !deb_change_indices.is_empty() {
        return Ok(true);
    }
    Ok(!NativeLifecycleResidualState::find_all(conn)?.is_empty())
}

fn host_capabilities_for_plan(
    conn: &rusqlite::Connection,
    arch_transaction: bool,
    plan: &NativeTransactionPlan,
) -> Result<Option<conary_core::ccs::HostCapabilityInventory>> {
    (arch_transaction || plan.requires_sysusers_interface())
        .then(|| conary_core::ccs::HostCapabilityInventory::load_required(conn))
        .transpose()
        .context("native transaction typed host capability preflight failed")
}

fn debian_relation_removal_operation(
    conn: &rusqlite::Connection,
    removal: &PackageRelationRemoval,
    trove_id: i64,
    inputs: &[NativeInstallInput<'_>],
    old_paths: &BTreeSet<String>,
) -> Result<NativeTransactionOperation> {
    let authorized_packages = match removal.mode {
        PackageRelationRemovalMode::Constraint => &removal.incoming_packages,
        PackageRelationRemovalMode::OwnershipTransfer => &removal.ownership_transfer_packages,
    };
    let counterpart_index = removal.triggering_incoming.transaction_index;
    let counterpart = inputs.get(counterpart_index).with_context(|| {
        format!(
            "Debian relation removal for '{} {}' refers to missing installing transaction element {counterpart_index}",
            removal.package_name, removal.package_version
        )
    })?;
    if counterpart.package_name != removal.triggering_incoming.package_name
        || counterpart.package_version != removal.triggering_incoming.package_version
        || counterpart.package_arch.map(str::to_string)
            != removal.triggering_incoming.package_architecture
        || authorized_packages
            .binary_search(&removal.triggering_incoming.package_name)
            .is_err()
    {
        anyhow::bail!(
            "Debian relation removal for '{} {}' carries an inconsistent installing transaction identity",
            removal.package_name,
            removal.package_version
        );
    }

    if removal.mode == PackageRelationRemovalMode::OwnershipTransfer {
        let config_paths = ConfigFile::find_by_trove(conn, trove_id)?
            .into_iter()
            .map(|config| config.path)
            .collect::<BTreeSet<_>>();
        let non_config_paths = old_paths
            .difference(&config_paths)
            .cloned()
            .collect::<BTreeSet<_>>();
        if !non_config_paths.is_empty() {
            let incoming_paths = counterpart.paths.iter().cloned().collect::<BTreeSet<_>>();
            if non_config_paths.is_subset(&incoming_paths) {
                return Ok(NativeTransactionOperation::Disappear {
                    overwriter_transaction_index: counterpart_index,
                });
            }
        }
    }

    Ok(NativeTransactionOperation::RemoveInFavour {
        replacement_transaction_index: counterpart_index,
    })
}

fn archive_paths_contain(paths: &BTreeSet<String>, expected: &str) -> bool {
    paths
        .iter()
        .any(|path| path.trim_start_matches('/') == expected)
}

fn executor_for_owner(owner: &NativeBundleOwner, root: &Path) -> Result<ScriptletExecutor> {
    let format = match &owner.bundle.source_format {
        conary_core::ccs::native_lifecycle::SourceFormat::Rpm => {
            conary_core::scriptlet::PackageFormat::Rpm
        }
        conary_core::ccs::native_lifecycle::SourceFormat::Deb => {
            conary_core::scriptlet::PackageFormat::Deb
        }
        conary_core::ccs::native_lifecycle::SourceFormat::Arch => {
            conary_core::scriptlet::PackageFormat::Arch
        }
        conary_core::ccs::native_lifecycle::SourceFormat::Eopkg => {
            conary_core::scriptlet::PackageFormat::Eopkg
        }
    };
    let executor =
        ScriptletExecutor::new(root, &owner.package_name, &owner.package_version, format)
            // Native package-manager lifecycle behavior must persist. The install
            // transaction owns failure handling and recovery.
            .with_sandbox_mode(SandboxMode::Always);
    Ok(executor)
}

#[cfg(test)]
#[path = "native_events/tests.rs"]
mod tests;
