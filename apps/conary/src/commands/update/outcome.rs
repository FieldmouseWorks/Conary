// apps/conary/src/commands/update/outcome.rs
//! Observations from the existing update planner and execution counters.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpdateOutcome {
    NoChanges,
    Planned { packages: usize },
    Applied { packages: usize },
}

pub(crate) enum CollectionUpdateStatus {
    Completed(UpdateOutcome),
    /// The request failed; this does not assert that it rolled back every effect.
    Failed,
}

pub(crate) struct CollectionUpdateEntry {
    pub target: String,
    pub status: CollectionUpdateStatus,
}
