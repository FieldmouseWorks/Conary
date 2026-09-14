// apps/conary/src/ui/diagnostics/repository.rs
//! Source remedies derive from retained typed synchronization failures.

use super::Diagnostic;
use crate::commands::RepositorySyncError;
use crate::ui::transaction_summary::{database_command, visible};
use conary_core::Error;

pub(super) fn from_error(error: &anyhow::Error) -> Option<Diagnostic> {
    Some(match error.downcast_ref::<RepositorySyncError>()? {
        RepositorySyncError::Unknown { database, name } => {
            let database = database.to_string_lossy();
            Diagnostic::new("Repository not found.")
                .fact("Database", database.as_ref())
                .fact("Repository", name)
                .note(format!(
                    "Run: {}",
                    database_command("conary repo list --all", &database)
                ))
        }
        RepositorySyncError::Failed { database, failures } => {
            let database = database.to_string_lossy();
            let mut diagnostic = Diagnostic::new("Repository metadata synchronization failed.")
                .fact("Database", database.as_ref());
            for failure in failures {
                diagnostic = diagnostic.fact("Repository", &failure.name);
                diagnostic = match &failure.cause {
                    Error::HttpStatus { status, url } => diagnostic
                        .fact("HTTP status", status.to_string())
                        .fact("Metadata URL", url),
                    Error::RepositoryResponseBody { url, detail } => {
                        diagnostic.fact("Metadata URL", url).fact("Cause", detail)
                    }
                    cause => diagnostic.fact("Cause", cause.to_string()),
                };
            }
            diagnostic = diagnostic
                .note("After resolving the reported causes, retry the failed repositories:");
            for failure in failures {
                let note = if database.chars().any(char::is_control)
                    || failure.name.chars().any(char::is_control)
                {
                    format!(
                        "Use 'conary repo sync --force' with repository name {} and the same database path.",
                        visible(&failure.name)
                    )
                } else {
                    format!(
                        "Run: {} -- '{}'",
                        database_command("conary repo sync --force", &database),
                        failure.name.replace('\'', "'\"'\"'")
                    )
                };
                diagnostic = diagnostic.note(note);
            }
            diagnostic
        }
    })
}
