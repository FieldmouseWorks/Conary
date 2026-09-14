// apps/conary/src/ui/diagnostics/repository.rs
//! Source remedies derive from retained typed synchronization failures.

use super::Diagnostic;
use crate::commands::{RepositoryCommandContext, RepositoryOperation, RepositorySyncError};
use crate::ui::transaction_summary::{database_command, visible};
use conary_core::Error;

pub(super) fn from_error(error: &anyhow::Error) -> Option<Diagnostic> {
    if let Some(context) = error.downcast_ref::<RepositoryCommandContext>() {
        return Some(operation_failure(error, context));
    }
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

fn operation_failure(error: &anyhow::Error, context: &RepositoryCommandContext) -> Diagnostic {
    let database = context.database.to_string_lossy();
    let mut diagnostic = Diagnostic::new(match context.operation {
        RepositoryOperation::Add => "Repository enrollment failed.",
        RepositoryOperation::Enable => "Repository enable failed.",
        RepositoryOperation::Disable => "Repository disable failed.",
        RepositoryOperation::Remove => "Repository removal failed.",
        RepositoryOperation::ResetTrust => "Repository trust reset failed.",
    })
    .fact("Database", database.as_ref())
    .fact("Repository", &context.name);
    // Context labels and every underlying cause remain inspectable. The UI
    // escapes their bytes; it does not infer authority from their wording.
    for cause in error.chain().skip(1) {
        diagnostic = diagnostic.fact("Cause", cause.to_string());
    }
    if matches!(
        error.downcast_ref::<Error>(),
        Some(Error::NotFound(_) | Error::ConflictError(_))
    ) {
        diagnostic = diagnostic.note("Inspect the configured repositories:");
        diagnostic = if database.chars().any(char::is_control) {
            diagnostic
                .note("Use 'conary repo list --all' with --db-path set to the same database path.")
        } else {
            diagnostic.note(format!(
                "Run: {}",
                database_command("conary repo list --all", &database)
            ))
        };
    }
    diagnostic
}
