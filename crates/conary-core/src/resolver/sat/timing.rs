// crates/conary-core/src/resolver/sat/timing.rs

//! Optional diagnostic timing; never resolution or publication authority.

use std::time::Instant;

#[derive(Debug, Clone, Copy)]
pub(super) enum Phase {
    Preparation,
    Initialization,
    Installed,
    Canonical,
    Transitive,
    Compilation,
    Solve,
    Classification,
}

pub(super) struct PhaseTimer {
    root_id: Option<i64>,
    phase: Phase,
    started: Instant,
}

pub(super) fn start(root_id: impl Into<Option<i64>>, phase: Phase) -> Option<PhaseTimer> {
    tracing::enabled!(target: "conary_core::resolver::timing", tracing::Level::DEBUG).then(|| {
        PhaseTimer {
            root_id: root_id.into(),
            phase,
            started: Instant::now(),
        }
    })
}

impl Drop for PhaseTimer {
    fn drop(&mut self) {
        tracing::debug!(
            target: "conary_core::resolver::timing",
            root_id = ?self.root_id,
            phase = ?self.phase,
            elapsed_seconds = self.started.elapsed().as_secs_f64(),
            "Resolver phase completed"
        );
    }
}
