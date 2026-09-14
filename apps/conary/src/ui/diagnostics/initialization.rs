// apps/conary/src/ui/diagnostics/initialization.rs
//! Initialization remedies come from the retained database error variant.

use super::Diagnostic;
use crate::commands::DatabaseInitializationContext;
use crate::ui::transaction_summary::database_command;
use conary_core::Error;
use std::path::Path;

pub(super) fn from_error(error: &anyhow::Error) -> Option<Diagnostic> {
    let context = error.downcast_ref::<DatabaseInitializationContext>()?;
    let source = error.downcast_ref::<Error>()?;
    let database = context.database.to_string_lossy();
    let parent = context.database.parent().unwrap_or_else(|| Path::new("."));
    let diagnostic = Diagnostic::new(match source {
        Error::SchemaRebuildRequired { .. } => "Database initialization requires a schema rebuild.",
        _ => "Database initialization failed.",
    })
    .fact("Database", database.as_ref())
    .fact("Database parent", parent.to_string_lossy())
    .fact("Runtime root", context.runtime_root.to_string_lossy());
    Some(match source {
        Error::SchemaRebuildRequired {
            observed,
            supported_epoch,
            supported_revision,
        } => diagnostic
            .fact("Observed schema", observed)
            .fact("Supported epoch", supported_epoch)
            .fact("Supported revision", supported_revision.to_string())
            .note("Rebuilding replaces active Conary state after preserving a snapshot. Use it only when this state is disposable.")
            .note(format!(
                "Run: {}",
                database_command("conary system rebuild-db --discard-state --yes", &database)
            )),
        other => diagnostic
            .fact("Cause", other.to_string())
            .note("Check that the database parent is a writable directory, or use --db-path with a writable location.")
            .note("Preserve existing Conary runtime state unless you have confirmed it is disposable."),
    })
}
