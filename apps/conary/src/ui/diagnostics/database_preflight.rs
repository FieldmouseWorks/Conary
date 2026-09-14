// apps/conary/src/ui/diagnostics/database_preflight.rs
//! Database-open facts retained before command-specific dispatch.

use super::Diagnostic;
use crate::dispatch::DatabasePreflightContext;
use conary_core::Error;

pub(super) fn from_error(error: &anyhow::Error) -> Option<Diagnostic> {
    let context = error.downcast_ref::<DatabasePreflightContext>()?;
    let database = context.database.to_string_lossy();
    if let Some(Error::SchemaRebuildRequired {
        observed,
        supported_epoch,
        supported_revision,
    }) = error.downcast_ref::<Error>()
    {
        return Some(
            Diagnostic::new("Database requires a schema rebuild.")
                .fact("Database", database.as_ref())
                .fact("Observed schema", observed)
                .fact("Supported epoch", supported_epoch)
                .fact("Supported revision", supported_revision.to_string())
                .note("Preserve existing Conary runtime state unless you have confirmed it is disposable.")
                // Common preflight has not validated rebuild target privileges
                // or canonical aliases. Offer help, never an unvalidated apply.
                .note("Run: conary system rebuild-db --help")
                .note("Any rebuild must select this same database with --db-path and satisfy the command's target and privilege checks."),
        );
    }
    let mut diagnostic =
        Diagnostic::new("Database preflight failed.").fact("Database", database.as_ref());
    for cause in error.chain().skip(1) {
        diagnostic = diagnostic.fact("Cause", cause.to_string());
    }
    Some(diagnostic.note("Check the selected database path and reported cause. Preserve existing Conary runtime state before attempting recovery."))
}
