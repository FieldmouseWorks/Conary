// apps/conary/src/commands/update/package/preview.rs

//! Artifact-backed update previews use the install planner without publishing state.

use super::*;

pub(super) struct PreparedUpdatePreview {
    pub(super) report: InstallReport,
    packages: std::collections::HashMap<i64, PreparedFullUpdate>,
    _temporary: tempfile::TempDir,
}

impl PreparedUpdatePreview {
    pub(super) fn replacement_for(
        &self,
        trove: &Trove,
    ) -> Result<&crate::commands::install::InstallReplacement> {
        let id = trove
            .id
            .context("selected update has no installed identity")?;
        self.packages
            .get(&id)
            .map(|package| &package.replacement)
            .context("selected update has no retained replacement authority")
    }

    pub(super) fn take_package(&mut self, trove: &Trove) -> Result<PreparedFullUpdate> {
        self.packages
            .remove(
                &trove
                    .id
                    .context("selected update has no installed identity")?,
            )
            .with_context(|| {
                format!(
                    "selected update artifact for {} was not retained",
                    trove.name
                )
            })
    }
}

pub(super) async fn plan_selected_updates(
    conn: &rusqlite::Connection,
    selected: &[(Trove, SelectedUpdateCandidate)],
    policy: &ResolutionPolicy,
    options: InstallOptions<'_>,
) -> Result<PreparedUpdatePreview> {
    // Resolution may hydrate a CCS artifact into CAS. A preview owns disposable
    // downloads and objects; it must not populate the installed runtime's CAS.
    let temporary = tempfile::tempdir()?;
    let projection = std::sync::Arc::new(crate::commands::install::preview::PreviewDatabase::new(
        conn,
        options.db_path,
    )?);
    let mut report = InstallReport {
        projection: Some(projection.clone()),
        ..Default::default()
    };
    // Establish that every requested snapshot exists in the initial projection
    // before earlier planned relation effects can legitimately remove one.
    let initial = conary_core::db::open(projection.path())?;
    for (trove, _) in selected {
        crate::commands::install::revalidate_replacement_snapshot(&initial, trove)?;
    }
    drop(initial);
    let mut packages = std::collections::HashMap::new();
    let mut ordered = selected
        .iter()
        .map(|entry| {
            let has_delta = PackageDelta::find_delta(
                conn,
                &entry.0.name,
                &entry.0.version,
                &entry.1.package.version,
            )?
            .is_some();
            Ok((!has_delta, entry))
        })
        .collect::<Result<Vec<_>>>()?;
    // Apply executes deltas before full downloads, retaining selection order
    // within each group. A failed delta uses its retained full artifact
    // immediately, so fallback preserves this same package sequence.
    ordered.sort_by_key(|(full, _)| *full);
    for (_, (trove, candidate)) in ordered {
        let resolution = resolution_options_for_selected_update(
            &candidate.package,
            &candidate.repository,
            temporary.path(),
            &temporary.path().join("objects"),
            policy,
        )?;
        let source = resolve_package(conn, &trove.name, &resolution)
            .await
            .with_context(|| format!("failed to resolve update preview for {}", trove.name))?;
        let path = source.path().ok_or_else(|| {
            anyhow::anyhow!("selected update for {} has no preview artifact", trove.name)
        })?;
        preflight_prepared_full_update_native_lifecycle(
            conn,
            trove,
            &candidate.package,
            &candidate.repository,
            path,
            options.db_path,
        )?;
        let pkg_path = path.to_path_buf();
        let projected = conary_core::db::open(projection.path())?;
        let id = trove
            .id
            .context("selected update has no installed identity")?;
        let replacement = if Trove::find_by_id(&projected, id)?.is_some() {
            crate::commands::install::InstallReplacement::Existing(trove.clone())
        } else {
            // The private projection started with this exact selected row. Its
            // absence here is an effect of an earlier admitted package, not a
            // fallback for unexpected drift in the real installed database.
            crate::commands::install::InstallReplacement::PlannedAbsent(trove.clone())
        };
        drop(projected);
        cmd_install_with_report(
            &path.to_string_lossy(),
            InstallOptions {
                db_path: projection.path(),
                replacement: Some(replacement.clone()),
                repository_provenance: Some(repository_install_provenance_from_package(
                    &candidate.package,
                    &candidate.repository,
                )?),
                ..options.clone()
            },
            &mut report,
        )
        .await?;
        let id = trove
            .id
            .context("selected update has no installed identity")?;
        if packages
            .insert(
                id,
                PreparedFullUpdate {
                    trove: trove.clone(),
                    replacement,
                    repo_pkg: candidate.package.clone(),
                    repo: candidate.repository.clone(),
                    pkg_path,
                    _source: source,
                },
            )
            .is_some()
        {
            anyhow::bail!("update selected installed identity {id} more than once");
        }
    }
    Ok(PreparedUpdatePreview {
        report,
        packages,
        _temporary: temporary,
    })
}
