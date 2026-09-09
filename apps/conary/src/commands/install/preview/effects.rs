// apps/conary/src/commands/install/preview/effects.rs
//! Database effects at native graph boundaries during disposable preview.

use super::super::batch::PreparedPackage;
use anyhow::{Context, Result};
use conary_core::ccs::native_transaction::DebPackageState;
use conary_core::db::models::{InstalledNativeLifecycleBundle, InstalledRequirementGroup};

pub(super) fn insert_package(
    tx: &rusqlite::Transaction<'_>,
    changeset_id: i64,
    package: &PreparedPackage,
) -> Result<i64> {
    let mut trove = package.to_trove(changeset_id)?;
    let id = trove.insert(tx)?;
    InstalledRequirementGroup::insert_groups(tx, id, trove.version_scheme, &package.requirements)?;
    InstalledRequirementGroup::insert_groups(tx, id, trove.version_scheme, &package.relations)?;
    super::super::transaction::persist_declared_provides(
        tx,
        id,
        &package.name,
        &package.version,
        trove.version_scheme,
        &package.provides,
    )?;
    if let Some(bundle) = package.native_lifecycle_state.bundle_to_persist.as_ref() {
        InstalledNativeLifecycleBundle::new(id, Some(changeset_id), bundle)?
            .insert_or_replace(tx)?;
    }
    conary_core::repository::enrollment::transaction::apply_transition(
        tx,
        package.old_trove_id()?,
        id,
        &package.name,
        &package.version,
        &package.repository_enrollments,
    )
    .context("Failed to project package repository enrollment")?;
    Ok(id)
}

pub(super) fn deconfigure(
    tx: &rusqlite::Transaction<'_>,
    change: &conary_core::transaction::PackageRelationDeconfiguration,
) -> Result<()> {
    let mut installed = InstalledNativeLifecycleBundle::find_by_trove(tx, change.package.trove_id)?
        .context("planned deconfiguration has no installed native lifecycle contract")?;
    let bundle = installed.bundle()?;
    anyhow::ensure!(
        bundle.source_format == conary_core::ccs::native_lifecycle::SourceFormat::Deb,
        "planned deconfiguration requires Debian's native lifecycle contract"
    );
    installed.set_lifecycle_state(DebPackageState::Unpacked);
    installed.insert_or_replace(tx)?;
    Ok(())
}
