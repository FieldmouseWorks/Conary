// apps/conary/src/ui/initialization.rs
//! Initialization frames retain the database selected by the command.

use super::transaction_summary::{database_command, visible};
use conary_core::db::rebuild::DatabaseRebuildOutcome;

pub(crate) fn database_initialized(db_path: &str) {
    super::status("Initialized", "Conary database");
    super::field("Database", &visible(db_path));
}

pub(crate) fn database_rebuilt(db_path: &str, outcome: &DatabaseRebuildOutcome) {
    super::status("Rebuilt", "Conary database with the current schema");
    super::field("Database", &visible(db_path));
    super::field("Retired schema", &visible(&outcome.observed_schema));
    super::field(
        "Retired snapshot",
        &visible(&outcome.retired_snapshot_path.to_string_lossy()),
    );
}

pub(crate) fn configuration_complete(db_path: &str) {
    super::status("Configured", "built-in Remi package source feeds");
    super::status(
        "Discovered",
        "typed host lifecycle interfaces (service manager, sysusers, tmpfiles, sysctl, ldconfig)",
    );
    super::note("Download metadata from every enabled Remi feed:");
    super::note(&format!(
        "Run: {}",
        database_command("conary repo sync", db_path)
    ));
    super::note("Enroll native sources with exact trust, identity, stream, and update policy.");
}
