// apps/conary/src/commands/install/preview.rs
//! Disposable installed-package state for ordered update planning.

mod effects;

use super::batch::PreparedPackage;
use anyhow::{Context, Result};
use conary_core::db::models::{Changeset, PackagePayloadOwnership, Trove};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Declared paths for projected packages, without claiming resolved owners or
/// materialized files. Existing packages retain their installed payload authority.
#[derive(Clone, Default)]
pub(super) struct DeclaredPayloadPaths {
    packages: BTreeMap<i64, BTreeSet<String>>,
}

impl DeclaredPayloadPaths {
    pub(super) fn for_trove(
        &self,
        conn: &rusqlite::Connection,
        id: i64,
    ) -> Result<BTreeSet<String>> {
        match self.packages.get(&id) {
            Some(paths) => Ok(paths.clone()),
            None => Ok(PackagePayloadOwnership::load(conn, id)?
                .lifecycle_paths()
                .iter()
                .cloned()
                .collect()),
        }
    }

    pub(super) fn installed_paths_excluding(
        &self,
        conn: &rusqlite::Connection,
        excluded: &HashSet<i64>,
    ) -> Result<BTreeSet<String>> {
        let mut paths = PackagePayloadOwnership::installed_paths_excluding(conn, excluded)?;
        for (id, declared) in &self.packages {
            if !excluded.contains(id) {
                paths.extend(declared.iter().cloned());
            }
        }
        Ok(paths)
    }
}

/// Only the private temporary database can receive projected package facts.
/// Lifecycle programs, selected-root writes, and publication never run here.
pub(crate) struct PreviewDatabase {
    _temporary: tempfile::TempDir,
    path: String,
    keyring_dir: PathBuf,
    declared_paths: Mutex<DeclaredPayloadPaths>,
}

impl PreviewDatabase {
    pub(crate) fn new(conn: &rusqlite::Connection, runtime_db_path: &str) -> Result<Self> {
        let temporary = tempfile::tempdir()?;
        let path = temporary.path().join("conary.db");
        conn.backup(rusqlite::MAIN_DB, &path, None)?;
        Ok(Self {
            path: path
                .to_str()
                .context("preview database path is not UTF-8")?
                .into(),
            keyring_dir: conary_core::db::paths::keyring_dir(runtime_db_path),
            declared_paths: Mutex::default(),
            _temporary: temporary,
        })
    }

    pub(crate) fn path(&self) -> &str {
        &self.path
    }

    pub(super) fn keyring_dir(&self) -> &Path {
        &self.keyring_dir
    }

    pub(super) fn declared_paths(&self) -> Result<DeclaredPayloadPaths> {
        Ok(self
            .declared_paths
            .lock()
            .map_err(|_| anyhow::anyhow!("preview path state poisoned"))?
            .clone())
    }

    pub(super) fn project(&self, packages: &[PreparedPackage]) -> Result<()> {
        use conary_core::ccs::native_transaction::NativeTransactionStep;
        use conary_core::repository::enrollment::transaction as enrollment;

        let mut declared_paths = self
            .declared_paths
            .lock()
            .map_err(|_| anyhow::anyhow!("preview path state poisoned"))?;
        let mut projected = declared_paths.clone();
        let mut conn = super::super::open_db(&self.path)?;
        let inputs = packages
            .iter()
            .map(PreparedPackage::native_install_input)
            .collect::<Vec<_>>();
        let native =
            super::native_events::PreparedNativeTransaction::prepare_batch_with_declared_paths(
                &conn, &inputs, &projected,
            )?;
        let finalization_troves = super::batch::finalization_trove_ids(packages)?;
        let removing_deb_identities = finalization_troves
            .iter()
            .map(|id| {
                id.map(|id| super::native_events::deb_identity_for_trove(&conn, id))
                    .transpose()
                    .map(Option::flatten)
            })
            .collect::<Result<Vec<_>>>()?;
        let relation_removals = packages
            .iter()
            .flat_map(|package| &package.relation_removals)
            .map(|removal| removal.trove_id)
            .collect::<BTreeSet<_>>();
        let deconfigurations = packages
            .iter()
            .flat_map(|package| &package.relation_deconfigurations)
            .collect::<Vec<_>>();
        let transitions = packages
            .iter()
            .map(|package| {
                Ok((
                    package.old_trove_id()?,
                    package.repository_enrollments.as_slice(),
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        enrollment::preflight_batch(&conn, &transitions)?;
        for id in &relation_removals {
            enrollment::preflight_removal(&conn, *id)?;
        }
        let tx = conn.transaction()?;
        let changeset_id =
            Changeset::new("Disposable update preview projection".into()).insert(&tx)?;
        // Deconfiguration is a lifecycle outcome without its own payload node.
        // Retain the declared success projection before any target is replaced.
        for change in deconfigurations {
            effects::deconfigure(&tx, change)?;
        }
        // Replay the source-owned payload boundaries. Lifecycle programs and
        // trigger execution never run; these are projected database facts only.
        for step in native.graph_steps() {
            match *step {
                NativeTransactionStep::ApplyPayload { change_index } => {
                    if let Some(package) = packages.get(change_index) {
                        let id = effects::insert_package(&tx, changeset_id, package)?;
                        if let Some(bundle) = package
                            .native_lifecycle_state
                            .bundle_to_persist
                            .as_ref()
                            .filter(|bundle| {
                                bundle.source_format
                                    == conary_core::ccs::native_lifecycle::SourceFormat::Deb
                            })
                        {
                            native.mark_install_payload_applied_for(
                                &tx,
                                &conary_core::ccs::native_transaction::NativePackageIdentity::new(
                                    &package.name,
                                    &package.version,
                                    bundle.source_arch.as_deref(),
                                ),
                            )?;
                        }
                        projected.packages.insert(
                            id,
                            package
                                .extracted_files
                                .iter()
                                .map(|file| file.path.clone())
                                .collect(),
                        );
                    }
                }
                NativeTransactionStep::FinalizeOldPayload { change_index } => {
                    if let Some(Some(id)) = finalization_troves.get(change_index) {
                        if relation_removals.contains(id) {
                            enrollment::apply_removal(&tx, *id)?;
                        }
                        if let Some(Some(identity)) = removing_deb_identities.get(change_index) {
                            native.mark_remove_payload_started_for(&tx, identity)?;
                        }
                        Trove::delete(&tx, *id)?;
                        if let Some(Some(identity)) = removing_deb_identities.get(change_index) {
                            native.mark_remove_payload_completed_for(
                                &tx,
                                identity,
                                change_index,
                            )?;
                        }
                        projected.packages.remove(id);
                    }
                }
                NativeTransactionStep::RunEvent { event_index } => {
                    native.project_graph_event_success(&tx, event_index)?;
                }
                NativeTransactionStep::PersistDebTriggerActivations {
                    transaction_index,
                    boundary,
                    ..
                } => {
                    native.persist_trigger_activations_for(&tx, transaction_index, boundary)?;
                }
                NativeTransactionStep::PurgeConfigFiles { .. } => {}
            }
        }
        native.finalize_successful_debian_installs(&tx)?;
        tx.commit()?;
        *declared_paths = projected;
        Ok(())
    }
}
