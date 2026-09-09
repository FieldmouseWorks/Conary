// apps/conary/src/commands/update/package.rs

//! Single-package update command execution.

use super::super::install::{
    CcsEnvelopeAuthority, OwnershipMode, repository_install_provenance_from_package,
    verify_ccs_package_authority,
};
use super::super::progress::{UpdatePhase, UpdateProgress};
use super::super::{InstallOptions, SandboxMode, open_db};
use super::adopted_authority::{
    AdoptedUpdateDecision, AdoptedUpdateSkip, AdoptedUpdateSkipReason, adopted_update_decision,
    native_manager_for_trove, no_update_message, render_adopted_skip_sample,
};
use super::selection::{
    SecurityMetadataUnavailable, SelectedUpdateCandidate, UpdateCandidateSelection,
    installed_troves_for_update, print_security_metadata_unavailable,
    render_security_update_marker, security_metadata_unavailable_error, select_update_candidate,
};
use crate::commands::install::{
    cmd_install_with_report,
    report::{InstallReport, PackageIdentity},
};
use anyhow::{Context, Result};
use conary_core::ccs::CcsPackage;
use conary_core::db::models::{DeltaStats, Repository, RepositoryPackage, Trove};
use conary_core::repository::{
    PackageSource, ResolutionOptions, resolution_policy::ResolutionPolicy, resolve_package,
};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

mod preview;

fn resolution_options_for_selected_update(
    repo_pkg: &RepositoryPackage,
    repo: &Repository,
    temp_dir: &Path,
    objects_dir: &Path,
    policy: &ResolutionPolicy,
) -> Result<ResolutionOptions> {
    let mut transaction_policy = policy.clone();
    let exact_source_identity = conary_core::repository::selector::candidate_source_identity(
        repo_pkg, repo,
    )?
    .ok_or_else(|| {
        anyhow::anyhow!(
            "selected update package '{}-{}' has no exact source identity",
            repo_pkg.name,
            repo_pkg.version
        )
    })?;
    transaction_policy.set_primary_source_identity(Some(exact_source_identity.to_string()));
    transaction_policy
        .validate_source_identities()
        .map_err(anyhow::Error::msg)?;
    Ok(ResolutionOptions {
        version: Some(repo_pkg.version.clone()),
        package_release: (!repo_pkg.package_release.is_empty())
            .then(|| repo_pkg.package_release.clone()),
        repository: Some(repo.name.clone()),
        architecture: repo_pkg.architecture.clone(),
        output_dir: Some(PathBuf::from(temp_dir)),
        objects_dir: Some(objects_dir.to_path_buf()),
        // Update has already selected a repository package. Do not let the
        // generic resolver short-circuit on the installed trove; execution
        // must consume the exact repository row selected by update planning.
        skip_installed: true,
        policy: Some(transaction_policy),
        is_root: false,
    })
}

fn mark_pending_changeset_rolled_back(
    conn: &mut rusqlite::Connection,
    changeset_id: i64,
) -> Result<bool> {
    use conary_core::db::models::{Changeset, ChangesetStatus};

    Ok(conary_core::db::transaction(conn, |tx| {
        let Some(mut changeset) = Changeset::find_by_id(tx, changeset_id)? else {
            return Ok(false);
        };

        if changeset.status != ChangesetStatus::Pending {
            return Ok(false);
        }

        changeset.update_status(tx, ChangesetStatus::RolledBack)?;
        Ok(true)
    })?)
}

use super::failure::{UpdateFailures, UpdatePackageFailure};

struct PreparedFullUpdate {
    trove: Trove,
    replacement: crate::commands::install::InstallReplacement,
    repo_pkg: RepositoryPackage,
    repo: Repository,
    pkg_path: PathBuf,
    _source: PackageSource,
}

#[allow(clippy::too_many_arguments)]
fn preflight_prepared_full_update_native_lifecycle(
    conn: &rusqlite::Connection,
    trove: &Trove,
    repo_pkg: &RepositoryPackage,
    repo: &Repository,
    pkg_path: &Path,
    db_path: &str,
) -> Result<()> {
    if pkg_path.extension().and_then(|ext| ext.to_str()) != Some("ccs") {
        return Ok(());
    }

    let repository_provenance = repository_install_provenance_from_package(repo_pkg, repo)?;
    let verified = verify_ccs_package_authority(
        db_path,
        pkg_path,
        &CcsEnvelopeAuthority::Repository,
        Some(&repository_provenance),
    )?;
    let pkg = CcsPackage::from_verified_archive(&pkg_path.to_string_lossy(), &verified)
        .with_context(|| format!("failed to parse selected update CCS {}", pkg_path.display()))?;
    if let Some(bundle) = pkg.manifest().native_lifecycle.as_ref() {
        bundle.validate()?;
    }
    if let Some(trove_id) = trove.id
        && let Some(installed) =
            conary_core::db::models::InstalledNativeLifecycleBundle::find_by_trove(conn, trove_id)?
    {
        installed.bundle()?;
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn install_options_for_update<'a>(
    db_path: &'a str,
    root: &'a str,
    sandbox_mode: SandboxMode,
    ownership: OwnershipMode,
    yes: bool,
    repo_pkg: &RepositoryPackage,
    repo: &Repository,
    replacement: &crate::commands::install::InstallReplacement,
) -> Result<InstallOptions<'a>> {
    Ok(InstallOptions {
        db_path,
        root,
        sandbox_mode,
        ownership: Some(ownership),
        yes,
        replacement: Some(replacement.clone()),
        repository_provenance: Some(repository_install_provenance_from_package(repo_pkg, repo)?),
        ..Default::default()
    })
}

/// Check for and apply package updates
///
/// If `security_only` is true, only applies updates from sources with trusted
/// advisory metadata that mark the candidate as a security update.
#[allow(clippy::too_many_arguments)]
pub async fn cmd_update(
    package: Option<String>,
    db_path: &str,
    root: &str,
    security_only: bool,
    dry_run: bool,
    sandbox_mode: SandboxMode,
    ownership: Option<OwnershipMode>,
    yes: bool,
    package_version: Option<String>,
    architecture: Option<String>,
) -> Result<()> {
    update_packages(
        package,
        db_path,
        root,
        security_only,
        dry_run,
        sandbox_mode,
        ownership,
        yes,
        package_version,
        architecture,
        false,
        None,
    )
    .await
    .map(|_| ())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn cmd_update_cli(
    package: Option<String>,
    db_path: &str,
    root: &str,
    security_only: bool,
    dry_run: bool,
    sandbox_mode: SandboxMode,
    ownership: Option<OwnershipMode>,
    yes: bool,
    package_version: Option<String>,
    architecture: Option<String>,
    release: Option<crate::commands::InstalledRelease>,
) -> Result<()> {
    update_packages(
        package,
        db_path,
        root,
        security_only,
        dry_run,
        sandbox_mode,
        ownership,
        yes,
        package_version,
        architecture,
        true,
        release,
    )
    .await
    .map(|_| ())
}

/// Retain selection/execution outcomes for callers that summarize several requests.
#[allow(clippy::too_many_arguments)]
pub(super) async fn update_packages(
    package: Option<String>,
    db_path: &str,
    root: &str,
    security_only: bool,
    dry_run: bool,
    sandbox_mode: SandboxMode,
    ownership: Option<OwnershipMode>,
    yes: bool,
    package_version: Option<String>,
    architecture: Option<String>,
    show_rollback: bool,
    release: Option<crate::commands::InstalledRelease>,
) -> Result<super::outcome::UpdateOutcome> {
    if security_only {
        info!("Checking for security updates only");
    } else {
        info!("Checking for package updates");
    }

    let requested_ownership = ownership;
    let ownership = requested_ownership.unwrap_or_default();

    let mut conn = open_db(db_path)?;
    let effective_source_policy = conary_core::repository::load_effective_policy(
        &conn,
        conary_core::repository::resolution_policy::RequestScope::Any,
    )?;
    let policy = effective_source_policy.resolution.clone();

    let installed_troves =
        installed_troves_for_update(&conn, package, package_version, architecture, release)?;

    if installed_troves.is_empty() {
        crate::ui::println!("No packages to update");
        return Ok(super::outcome::UpdateOutcome::NoChanges);
    }

    // Collect updates with their repository info (needed for GPG verification)
    let mut updates_available: Vec<(Trove, SelectedUpdateCandidate)> = Vec::new();
    let mut pinned_skipped: Vec<String> = Vec::new();

    let mut adopted_skipped: Vec<AdoptedUpdateSkip> = Vec::new();
    let mut security_metadata_unavailable: Vec<SecurityMetadataUnavailable> = Vec::new();

    for trove in &installed_troves {
        // Skip pinned packages
        if trove.pinned {
            pinned_skipped.push(trove.name.clone());
            continue;
        }

        let adopted_decision = if trove.install_source.is_adopted() {
            Some(adopted_update_decision(ownership, requested_ownership))
        } else {
            None
        };
        let enforce_security_metadata = security_only
            && !matches!(
                adopted_decision,
                Some(AdoptedUpdateDecision::SkipNativeAuthority)
            );

        let selected =
            match select_update_candidate(&conn, trove, enforce_security_metadata, &policy)? {
                UpdateCandidateSelection::Selected(selected) => *selected,
                UpdateCandidateSelection::NoEligibleUpdate => continue,
                UpdateCandidateSelection::SecurityMetadataUnavailable(unavailable) => {
                    security_metadata_unavailable.push(unavailable);
                    continue;
                }
            };

        // For adopted packages, native package-manager authority is preserved
        // unless the user explicitly asks Conary to take ownership.
        if trove.install_source.is_adopted() {
            let native_manager = native_manager_for_trove(trove);
            match adopted_decision.expect("adopted trove must have an update decision") {
                AdoptedUpdateDecision::SkipNativeAuthority => {
                    let guidance = native_manager.map_or_else(
                        || "use the recorded external owner".to_string(),
                        |manager| format!("use '{}'", manager.update_command(&trove.name)),
                    );
                    crate::ui::println!(
                        "  {} {} -> {} (adopted as {}, external authority: {})",
                        trove.name,
                        trove.version,
                        selected.package.version,
                        trove.install_source.as_str(),
                        guidance,
                    );
                    adopted_skipped.push(AdoptedUpdateSkip {
                        package: trove.name.clone(),
                        manager: native_manager,
                        reason: AdoptedUpdateSkipReason::NativeAuthority,
                    });
                    continue;
                }
                AdoptedUpdateDecision::QueueTakeover => {
                    crate::ui::println!(
                        "  {} {} -> {} (taking over from system PM)",
                        trove.name,
                        trove.version,
                        selected.package.version
                    );
                }
            }
        }

        let security_marker = render_security_update_marker(&selected.package);
        info!(
            "Update available: {} {} -> {}{}",
            trove.name, trove.version, selected.package.version, security_marker
        );
        updates_available.push((trove.clone(), selected));
    }

    if !security_metadata_unavailable.is_empty() {
        print_security_metadata_unavailable(&security_metadata_unavailable);
        anyhow::bail!(security_metadata_unavailable_error(
            security_metadata_unavailable.len()
        ));
    }

    // Report pinned packages that were skipped
    if !pinned_skipped.is_empty() {
        crate::ui::println!(
            "Skipping {} pinned package(s): {}",
            pinned_skipped.len(),
            pinned_skipped.join(", ")
        );
    }

    // Report adopted packages that were skipped because native authority still owns them.
    if !adopted_skipped.is_empty() {
        let native_authority: Vec<&AdoptedUpdateSkip> = adopted_skipped
            .iter()
            .filter(|skip| skip.reason == AdoptedUpdateSkipReason::NativeAuthority)
            .collect();
        if !native_authority.is_empty() {
            crate::ui::println!(
                "Skipping {} adopted package(s); native package-manager authority owns updates: {}",
                native_authority.len(),
                render_adopted_skip_sample(&native_authority)
            );
            crate::ui::println!(
                "Run 'conary system adopt --refresh' after native package-manager changes before retrying Conary workflows."
            );
            if !matches!(requested_ownership, Some(OwnershipMode::Takeover)) {
                crate::ui::println!(
                    "Use --ownership takeover to request Conary takeover for adopted packages."
                );
            }
        }
    }

    if updates_available.is_empty() {
        crate::ui::println!(
            "{}",
            no_update_message(security_only, !adopted_skipped.is_empty())
        );
        return Ok(super::outcome::UpdateOutcome::NoChanges);
    }

    let security_count = updates_available
        .iter()
        .filter(|(_, selected)| selected.package.is_security_update)
        .count();
    if security_only {
        crate::ui::println!(
            "Found {} security update(s) available:",
            updates_available.len()
        );
    } else {
        crate::ui::println!(
            "Found {} package(s) with updates available{}:",
            updates_available.len(),
            if security_count > 0 {
                format!(" ({} security)", security_count)
            } else {
                String::new()
            }
        );
    }
    let mut preview = preview::plan_selected_updates(
        &conn,
        &updates_available,
        &policy,
        InstallOptions {
            db_path,
            root,
            sandbox_mode,
            ownership: Some(ownership),
            dry_run: true,
            yes: true,
            ..Default::default()
        },
    )
    .await?;
    crate::ui::transaction_summary::install_preview(&preview.report.planned);
    for (_, selected) in &updates_available {
        if selected.package.is_security_update {
            crate::ui::message(&crate::ui::note_line(&format!(
                "{}{}",
                selected.package.name,
                render_security_update_marker(&selected.package)
            )));
        }
    }
    if dry_run {
        crate::ui::message(&crate::ui::note_line("Dry run: no updates were applied."));
        return Ok(super::outcome::UpdateOutcome::Planned {
            packages: updates_available.len(),
        });
    }
    let prepared_full_artifacts = i32::try_from(updates_available.len())
        .context("too many selected update artifacts for statistics")?;
    let targets: std::collections::HashSet<_> = updates_available
        .iter()
        .map(|(_, selected)| PackageIdentity::repository(&selected.package))
        .collect();
    let mut report = InstallReport::default();

    let mut required_failures: Vec<UpdatePackageFailure> = Vec::new();
    let total_requested = updates_available.len();
    // Planning already acquired and admitted every full artifact. Apply those
    // exact artifacts in their admitted order; a delta cannot avoid these bytes.
    let prepared_full_updates = preview.take_packages();
    let changeset_id = conary_core::db::transaction(&mut conn, |tx| {
        let mut changeset = conary_core::db::models::Changeset::new(format!(
            "Update {} package(s)",
            total_requested
        ));
        changeset.insert(tx)
    })?;

    let update_result: Result<super::outcome::UpdateOutcome> = async {
        let mut cancelled_package = None;
        'apply_updates: {
            // Install the retained artifacts in the exact preview order.
            // This respects per-repo routing strategies (remi, binary, etc.)
            if !prepared_full_updates.is_empty() {
                let total_to_install = prepared_full_updates.len() as u64;
                let mut progress = UpdateProgress::new(total_to_install);

                progress.set_status("Installing packages...");

                for PreparedFullUpdate {
                    trove,
                    replacement,
                    repo_pkg,
                    repo,
                    pkg_path,
                    _source,
                } in prepared_full_updates
                {
                    info!(
                        "Installing prepared update {} from {}",
                        trove.name, repo.name
                    );
                    progress.set_phase(&trove.name, UpdatePhase::Installing);

                    let path_str = pkg_path.to_string_lossy().to_string();

                    match cmd_install_with_report(
                        &path_str,
                        install_options_for_update(
                            db_path,
                            root,
                            sandbox_mode,
                            ownership,
                            yes,
                            &repo_pkg,
                            &repo,
                            &replacement,
                        )?,
                        &mut report,
                    )
                    .await
                    {
                        Ok(crate::commands::install::InstallOutcome::Cancelled) => {
                            cancelled_package = Some(trove.name.clone());
                            break 'apply_updates;
                        }
                        Ok(crate::commands::install::InstallOutcome::Completed) => {}
                        Err(e) => {
                            progress.fail_package(&trove.name, &e.to_string());
                            tracing::debug!("Package installation failed: {e:#}");
                            required_failures.push(UpdatePackageFailure {
                                package: trove.name.clone(),
                                version: repo_pkg.version.clone(),
                                error: e,
                            });
                            let _ = std::fs::remove_file(&pkg_path);
                            continue;
                        }
                    }

                    progress.complete_package(&trove.name);
                    let _ = std::fs::remove_file(&pkg_path);
                }

                progress.clear();
            }
        }
        conary_core::db::transaction(&mut conn, |tx| {
            let mut stats = DeltaStats::new(changeset_id);
            // Full artifacts were admitted before apply. No delta was fetched
            // and no bandwidth savings are claimed.
            stats.full_downloads = prepared_full_artifacts;
            stats.insert(tx)?;

            let mut changeset = conary_core::db::models::Changeset::find_by_id(tx, changeset_id)?
                .ok_or_else(|| {
                conary_core::Error::NotFound("Changeset not found".to_string())
            })?;
            if !report.commits.is_empty() {
                changeset.update_status(tx, conary_core::db::models::ChangesetStatus::Applied)?;
            } else if !required_failures.is_empty() || cancelled_package.is_some() {
                changeset
                    .update_status(tx, conary_core::db::models::ChangesetStatus::RolledBack)?;
            } else {
                changeset.update_status(tx, conary_core::db::models::ChangesetStatus::Applied)?;
            }

            Ok(())
        })?;

        crate::ui::heading("Update artifact results:");
        crate::ui::println!("Full artifacts prepared: {}", prepared_full_artifacts);
        if let Some(package) = cancelled_package {
            anyhow::bail!(
                "Update cancelled while installing {package}; remaining updates were not applied"
            );
        }
        if !required_failures.is_empty() {
            return Err(UpdateFailures {
                failures: required_failures,
                total_requested,
                committed_changesets: report
                    .commits
                    .iter()
                    .map(|commit| commit.changeset_id)
                    .collect(),
            }
            .into());
        }

        let packages = report.applied_targets(&targets);
        Ok(if packages == 0 {
            super::outcome::UpdateOutcome::NoChanges
        } else {
            super::outcome::UpdateOutcome::Applied { packages }
        })
    }
    .await;

    report.render(db_path, false);
    if show_rollback && update_result.is_ok() {
        crate::ui::transaction_summary::install_rollback_route(&report, db_path);
    }

    match update_result {
        Ok(outcome) => Ok(outcome),
        Err(err) => {
            if let Err(cleanup_err) = mark_pending_changeset_rolled_back(&mut conn, changeset_id) {
                warn!(
                    "Failed to mark abandoned update changeset {} as rolled back: {}",
                    changeset_id, cleanup_err
                );
            }
            Err(err)
        }
    }
}

#[cfg(test)]
mod tests;
