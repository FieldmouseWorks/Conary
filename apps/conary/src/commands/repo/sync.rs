// apps/conary/src/commands/repo/sync.rs

//! Repository metadata synchronization command.

use super::super::open_db;
use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;
use tracing::info;

/// Sync repository metadata
pub async fn cmd_repo_sync(name: Option<String>, db_path: &str, force: bool) -> Result<()> {
    info!("Synchronizing repository metadata");

    let conn = open_db(db_path)?;

    let repos_to_sync = if let Some(repo_name) = name {
        let repo = conary_core::db::models::Repository::find_by_name(&conn, &repo_name)?
            .ok_or_else(|| anyhow::anyhow!("Repository '{}' not found", repo_name))?;
        vec![repo]
    } else {
        conary_core::db::models::Repository::list_enabled(&conn)?
    };

    if repos_to_sync.is_empty() {
        crate::ui::message("No enabled repositories to sync.");
        let repos = conary_core::db::models::Repository::list_all(&conn)?;
        crate::ui::repository::metadata_guidance(&repos, db_path);
        return Ok(());
    }

    let repos_needing_sync: Vec<_> = repos_to_sync
        .into_iter()
        .filter(|repo| force || conary_core::repository::needs_sync(repo))
        .collect();

    if repos_needing_sync.is_empty() {
        println!("All repositories are up to date");
        return Ok(());
    }

    let spinner_style = ProgressStyle::default_spinner()
        .template("  {spinner:.cyan} {msg}")
        .expect("Invalid spinner template");

    let mut results: Vec<(String, conary_core::Result<usize>)> = Vec::new();
    for repo in &repos_needing_sync {
        let spinner = ProgressBar::new_spinner();
        spinner.set_style(spinner_style.clone());
        spinner.enable_steady_tick(Duration::from_millis(100));
        spinner.set_message(format!("Syncing metadata for {}...", repo.name));
        let sync_result = {
            let conn = conary_core::db::open(db_path)?;
            let mut repo_mut = repo.clone();
            conary_core::repository::sync_repository(&conn, &mut repo_mut).await
        };

        spinner.finish_and_clear();

        results.push((repo.name.clone(), sync_result));
    }

    let mut failures = Vec::new();

    for (name, result) in results {
        match result {
            Ok(count) => {
                let row = format!("Synchronized {count} packages from {name}");
                crate::ui::row(crate::ui::Status::Ok, &[&row]);
            }
            Err(e) => {
                let row = format!("Failed to sync {name}: {e}");
                crate::ui::row(crate::ui::Status::Fail, &[&row]);
                failures.push((name, e.to_string()));
            }
        }
    }

    if !failures.is_empty() {
        let failed_names = failures
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::bail!("Failed to sync repository metadata for: {failed_names}");
    }

    Ok(())
}
