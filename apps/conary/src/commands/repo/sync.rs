// apps/conary/src/commands/repo/sync.rs

//! Repository metadata synchronization command.

use super::super::open_db;
use anyhow::Result;
use conary_core::db::models::Repository;
use std::path::PathBuf;
use tracing::info;

#[derive(Debug, thiserror::Error)]
pub(crate) enum RepositorySyncError {
    #[error("Repository not found.")]
    Unknown { database: PathBuf, name: String },
    #[error("Repository metadata synchronization failed.")]
    Failed {
        database: PathBuf,
        failures: Vec<SourceSyncFailure>,
    },
}

#[derive(Debug)]
pub(crate) struct SourceSyncFailure {
    pub(crate) name: String,
    pub(crate) cause: conary_core::Error,
}

/// Sync repository metadata
pub async fn cmd_repo_sync(name: Option<String>, db_path: &str, force: bool) -> Result<()> {
    info!("Synchronizing repository metadata");

    let conn = open_db(db_path)?;

    let repos_to_sync = if let Some(repo_name) = name {
        let repo = Repository::find_by_name(&conn, &repo_name)?.ok_or_else(|| {
            RepositorySyncError::Unknown {
                database: db_path.into(),
                name: repo_name,
            }
        })?;
        vec![repo]
    } else {
        Repository::list_enabled(&conn)?
    };

    if repos_to_sync.is_empty() {
        crate::ui::repository::sync_empty(db_path);
        let repos = Repository::list_all(&conn)?;
        crate::ui::repository::metadata_guidance(&repos, db_path);
        return Ok(());
    }

    let repos_needing_sync: Vec<_> = repos_to_sync
        .into_iter()
        .filter(|repo| force || conary_core::repository::needs_sync(repo))
        .collect();

    if repos_needing_sync.is_empty() {
        crate::ui::repository::sync_not_due(db_path);
        return Ok(());
    }

    let progress = crate::ui::repository::SyncProgress::new(repos_needing_sync.len());
    let mut results: Vec<(String, conary_core::Result<usize>)> = Vec::new();
    for (index, repo) in repos_needing_sync.iter().enumerate() {
        progress.source(&repo.name);
        results.push((repo.name.clone(), sync_one(db_path, repo).await));
        progress.advance(index + 1);
    }
    drop(progress);

    report_results(db_path, results)
}

fn report_results(db_path: &str, results: Vec<(String, conary_core::Result<usize>)>) -> Result<()> {
    crate::ui::repository::sync_results(db_path, &results);
    let failures: Vec<_> = results
        .into_iter()
        .filter_map(|(name, result)| result.err().map(|cause| SourceSyncFailure { name, cause }))
        .collect();
    if !failures.is_empty() {
        return Err(RepositorySyncError::Failed {
            database: db_path.into(),
            failures,
        }
        .into());
    }

    Ok(())
}

async fn sync_one(db_path: &str, repo: &Repository) -> conary_core::Result<usize> {
    // A failed per-source database open is still an attempt result. Preserve
    // earlier successes and every original cause when reporting the batch.
    let conn = conary_core::db::open(db_path)?;
    let mut repo = repo.clone();
    conary_core::repository::sync_repository(&conn, &mut repo).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use conary_core::Error;

    #[test]
    fn batch_failure_retains_every_typed_cause_through_additional_context() {
        let error = report_results(
            "selected.db",
            vec![
                ("available".into(), Ok(4)),
                (
                    "http".into(),
                    Err(Error::HttpStatus {
                        status: 404,
                        url: "https://example.invalid/metadata.json".into(),
                    }),
                ),
                (
                    "database".into(),
                    Err(Error::DatabaseNotFound("selected.db".into())),
                ),
            ],
        )
        .unwrap_err()
        .context("caller context");
        let RepositorySyncError::Failed { database, failures } = error.downcast_ref().unwrap()
        else {
            panic!("expected typed batch failure")
        };
        assert_eq!(database, &PathBuf::from("selected.db"));
        assert_eq!(failures.len(), 2);
        assert_eq!(failures[0].name, "http");
        assert!(
            matches!(&failures[0].cause, Error::HttpStatus { status: 404, url } if url == "https://example.invalid/metadata.json")
        );
        assert_eq!(failures[1].name, "database");
        assert!(
            matches!(&failures[1].cause, Error::DatabaseNotFound(path) if path == "selected.db")
        );
    }

    #[tokio::test]
    async fn unknown_source_retains_selected_database_and_name() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("selected.db");
        conary_core::db::init(&database).unwrap();
        let error = cmd_repo_sync(Some("unknown".into()), database.to_str().unwrap(), false)
            .await
            .unwrap_err()
            .context("caller context");
        assert!(
            matches!(error.downcast_ref::<RepositorySyncError>(), Some(RepositorySyncError::Unknown { database: selected, name }) if selected == &database && name == "unknown")
        );
    }
}
