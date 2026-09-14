// apps/conary/src/commands/system/rebuild_database.rs

use super::init::{configure_current_database, require_init_privileges};
use anyhow::Result;
use std::path::Path;
use tracing::info;

/// Snapshot a retired pre-alpha database and replace its active state.
pub fn cmd_rebuild_database(db_path: &str) -> Result<()> {
    let db_path = Path::new(db_path);
    require_init_privileges(db_path)?;
    info!(database = %db_path.display(), "Rebuilding retired Conary database");

    let outcome = conary_core::db::rebuild::rebuild_discarding_state(db_path)?;
    crate::ui::initialization::database_rebuilt(&db_path.to_string_lossy(), &outcome);
    configure_current_database(db_path.to_string_lossy().as_ref())?;
    crate::ui::note(
        "Repository metadata and installed-package state were discarded; resync repositories and explicitly re-adopt any native packages that Conary should track.",
    );
    Ok(())
}
