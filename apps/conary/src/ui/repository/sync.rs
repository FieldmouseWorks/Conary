// apps/conary/src/ui/repository/sync.rs
//! Sync presentation consumes the command's per-source outcomes and core policy.

use crate::ui::progress::ProgressDisplay;
use crate::ui::transaction_summary::visible;
use crate::ui::{Status, field, heading, message, row};

pub(crate) struct SyncProgress(ProgressDisplay);

impl SyncProgress {
    pub(crate) fn new(total: usize) -> Self {
        Self(ProgressDisplay::new(
            total as u64,
            "Synchronizing repositories",
        ))
    }

    pub(crate) fn source(&self, name: &str) {
        self.0
            .set_status(format!("Checking metadata for {}", visible(name)));
    }

    pub(crate) fn advance(&self, completed: usize) {
        self.0.set_position(completed as u64);
    }
}

fn frame(db_path: &str) {
    heading("Repository synchronization:");
    field("Database", &visible(db_path));
}

pub(crate) fn sync_empty(db_path: &str) {
    frame(db_path);
    message("No enabled repositories to sync.");
}

pub(crate) fn sync_not_due(db_path: &str) {
    frame(db_path);
    message("No repository metadata checks are due.");
}

pub(crate) fn sync_results(db_path: &str, results: &[(String, conary_core::Result<usize>)]) {
    frame(db_path);
    for (name, result) in results {
        row(
            if result.is_ok() {
                Status::Ok
            } else {
                Status::Fail
            },
            &[&visible(name)],
        );
        if let Ok(count) = result {
            field("Package records synchronized", &count.to_string());
        }
    }
}
