// apps/conary/src/commands/install/preview.rs
//! Disposable installed-package state for ordered update planning.

use super::batch::PreparedPackage;
use anyhow::{Context, Result};
use conary_core::ccs::native_transaction::DebPackageState;
use conary_core::db::models::{
    Changeset, FileEntry, InstalledNativeLifecycleBundle, InstalledRequirementGroup, Trove,
};
use std::collections::BTreeSet;
use std::path::Path;

/// Only the private temporary database can receive projected package facts.
/// Lifecycle programs, selected-root writes, and publication never run here.
pub(crate) struct PreviewDatabase {
    _temporary: tempfile::TempDir,
    path: String,
    root: String,
}

impl PreviewDatabase {
    pub(crate) fn new(conn: &rusqlite::Connection, root: &str) -> Result<Self> {
        let temporary = tempfile::tempdir()?;
        let path = temporary.path().join("conary.db");
        conn.backup(rusqlite::MAIN_DB, &path, None)?;
        Ok(Self {
            path: path
                .to_str()
                .context("preview database path is not UTF-8")?
                .into(),
            root: root.into(),
            _temporary: temporary,
        })
    }

    pub(crate) fn path(&self) -> &str {
        &self.path
    }

    pub(super) fn project(&self, packages: &[PreparedPackage]) -> Result<()> {
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
            // Reuse the install payload authority to preserve exact paths,
            // ownership, kinds, and content identities for later relation and
            // native-lifecycle planning. CAS belongs to this disposable DB.
            let cas = conary_core::filesystem::CasStore::new(conary_core::db::paths::objects_dir(
                &self.path,
            ))?;
            let stored =
                super::inner::store_extracted_files_in_cas(&cas, &package.extracted_files)?;
            let files = super::inner::resolve_stored_install_files(
                Path::new(&self.root),
                &stored,
                package.semantics,
            )?;
            for file in files {
                FileEntry::new(file.path, file.node, file.content, id).insert_or_replace(
                    &tx,
                    conary_core::db::models::ExistingDirectoryMaterialization::ApplyIncoming,
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}
