// apps/conary/src/commands/install/preview.rs
//! Disposable installed-package state for ordered update planning.

use super::batch::PreparedPackage;
use anyhow::{Context, Result};
use conary_core::ccs::native_transaction::DebPackageState;
use conary_core::db::models::{
    Changeset, InstalledNativeLifecycleBundle, InstalledRequirementGroup, PackagePayloadOwnership,
    Trove,
};
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
        let mut declared_paths = self
            .declared_paths
            .lock()
            .map_err(|_| anyhow::anyhow!("preview path state poisoned"))?;
        let mut projected = declared_paths.clone();
        let mut conn = super::super::open_db(&self.path)?;
        let tx = conn.transaction()?;
        let mut changeset = Changeset::new("Disposable update preview projection".into());
        let changeset_id = changeset.insert(&tx)?;
        let mut removals = BTreeSet::new();
        for package in packages {
            removals.extend(package.old_trove_id()?);
            removals.extend(
                package
                    .relation_removals
                    .iter()
                    .map(|removal| removal.trove_id),
            );
        }
        for id in removals {
            Trove::delete(&tx, id)?;
            projected.packages.remove(&id);
        }
        for package in packages {
            for deconfiguration in &package.relation_deconfigurations {
                let mut installed = InstalledNativeLifecycleBundle::find_by_trove(
                    &tx,
                    deconfiguration.package.trove_id,
                )?
                .context("planned deconfiguration has no installed native lifecycle contract")?;
                let bundle = installed.bundle()?;
                anyhow::ensure!(
                    bundle.source_format == conary_core::ccs::native_lifecycle::SourceFormat::Deb,
                    "planned deconfiguration requires Debian's native lifecycle contract"
                );
                installed.set_lifecycle_state(DebPackageState::Unpacked);
                installed.insert_or_replace(&tx)?;
            }
            let mut trove = package.to_trove(changeset_id)?;
            let id = trove.insert(&tx)?;
            InstalledRequirementGroup::insert_groups(
                &tx,
                id,
                trove.version_scheme,
                &package.requirements,
            )?;
            InstalledRequirementGroup::insert_groups(
                &tx,
                id,
                trove.version_scheme,
                &package.relations,
            )?;
            super::transaction::persist_declared_provides(
                &tx,
                id,
                &package.name,
                &package.version,
                trove.version_scheme,
                &package.provides,
            )?;
            if let Some(bundle) = package.native_lifecycle_state.bundle_to_persist.as_ref() {
                InstalledNativeLifecycleBundle::new(id, Some(changeset_id), bundle)?
                    .insert_or_replace(&tx)?;
            }
            // Named identities can be created by pre-payload lifecycle programs.
            // Preview retains paths; apply resolves ownership after those programs.
            projected.packages.insert(
                id,
                package
                    .extracted_files
                    .iter()
                    .map(|file| file.path.clone())
                    .collect(),
            );
        }
        tx.commit()?;
        *declared_paths = projected;
        Ok(())
    }
}
