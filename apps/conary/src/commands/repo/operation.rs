// apps/conary/src/commands/repo/operation.rs
//! Requested operation context; core errors and mutation rules remain authoritative.

use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RepositoryOperation {
    Add,
    Enable,
    Disable,
    Remove,
    ResetTrust,
}

#[derive(Debug, thiserror::Error)]
#[error("Repository operation failed.")]
pub(crate) struct RepositoryCommandContext {
    pub(crate) operation: RepositoryOperation,
    pub(crate) name: String,
    pub(crate) database: PathBuf,
}

impl RepositoryCommandContext {
    pub(crate) fn new(operation: RepositoryOperation, name: &str, database: &str) -> Self {
        Self {
            operation,
            name: name.into(),
            database: database.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{cmd_repo_disable, cmd_repo_enable, cmd_repo_remove};

    #[test]
    fn failed_state_commands_retain_core_cause_and_exact_operation_context() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("state.db");
        conary_core::db::init(&database).unwrap();
        for (command, operation) in [
            (
                cmd_repo_enable as fn(&str, &str) -> anyhow::Result<()>,
                RepositoryOperation::Enable,
            ),
            (cmd_repo_disable, RepositoryOperation::Disable),
            (cmd_repo_remove, RepositoryOperation::Remove),
        ] {
            let error = command("unknown\nsource", database.to_str().unwrap())
                .unwrap_err()
                .context("caller context");
            assert!(matches!(
                error.downcast_ref::<conary_core::Error>(),
                Some(conary_core::Error::NotFound(_))
            ));
            let context = error.downcast_ref::<RepositoryCommandContext>().unwrap();
            assert_eq!(context.operation, operation);
            assert_eq!(context.name, "unknown\nsource");
            assert_eq!(context.database, database);
        }
    }

    #[tokio::test]
    async fn enrollment_insert_failure_retains_original_database_error_and_rolls_back() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("state.db");
        conary_core::db::init(&database).unwrap();
        let conn = conary_core::db::open(&database).unwrap();
        conn.execute_batch("CREATE TRIGGER reject_fixture_enrollment BEFORE INSERT ON repositories BEGIN SELECT RAISE(ABORT, 'fixture insert failure'); END;").unwrap();
        let error = crate::commands::cmd_repo_add(crate::commands::RepoAddOptions {
            name: "fixture".into(),
            url: "https://example.invalid".into(),
            package_format: Some(conary_core::repository::RepositoryFormat::Json),
            db_path: database.to_str().unwrap().into(),
            ..Default::default()
        })
        .await
        .unwrap_err()
        .context("caller context");
        assert!(
            matches!(error.downcast_ref::<conary_core::Error>(), Some(conary_core::Error::Database(rusqlite::Error::SqliteFailure(_, Some(message)))) if message == "fixture insert failure")
        );
        let context = error.downcast_ref::<RepositoryCommandContext>().unwrap();
        assert_eq!(context.operation, RepositoryOperation::Add);
        assert_eq!(context.name, "fixture");
        assert_eq!(context.database, database);
        assert!(
            conary_core::db::models::Repository::list_all(&conn)
                .unwrap()
                .is_empty()
        );
    }
}
