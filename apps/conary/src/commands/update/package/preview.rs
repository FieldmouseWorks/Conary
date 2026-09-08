// apps/conary/src/commands/update/package/preview.rs

//! Artifact-backed update previews use the install planner without publishing state.

use super::*;

pub(super) async fn plan_selected_updates(
    conn: &rusqlite::Connection,
    selected: &[(Trove, SelectedUpdateCandidate)],
    policy: &ResolutionPolicy,
    options: InstallOptions<'_>,
) -> Result<InstallReport> {
    // Resolution may hydrate a CCS artifact into CAS. A preview owns disposable
    // downloads and objects; it must not populate the installed runtime's CAS.
    let temporary = tempfile::tempdir()?;
    let mut report = InstallReport::default();
    for (trove, candidate) in selected {
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
        cmd_install_with_report(
            &path.to_string_lossy(),
            InstallOptions {
                repository_provenance: Some(repository_install_provenance_from_package(
                    &candidate.package,
                    &candidate.repository,
                )?),
                ..options.clone()
            },
            &mut report,
        )
        .await?;
    }
    Ok(report)
}
