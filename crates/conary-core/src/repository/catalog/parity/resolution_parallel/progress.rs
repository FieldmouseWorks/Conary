// crates/conary-core/src/repository/catalog/parity/resolution_parallel/progress.rs

//! Bounded observations of worker progress; never bundle or publication authority.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::super::contract::NativeParityOracleV1;

pub(super) const REPORT_INTERVAL: Duration = Duration::from_secs(30);
static NEXT_WALK: AtomicU64 = AtomicU64::new(1);

pub(super) struct Progress<'a> {
    manifest: &'a NativeParityOracleV1,
    walk_id: u64,
    workers: usize,
    started: Instant,
    last_report: Instant,
    pub(super) dispatched: u64,
    pub(super) completed: Arc<AtomicU64>,
    pub(super) emitted: u64,
}

impl<'a> Progress<'a> {
    pub(super) fn new(manifest: &'a NativeParityOracleV1, workers: usize) -> Self {
        let now = Instant::now();
        Self {
            manifest,
            walk_id: NEXT_WALK.fetch_add(1, Ordering::Relaxed),
            workers,
            started: now,
            last_report: now,
            dispatched: 0,
            completed: Arc::new(AtomicU64::new(0)),
            emitted: 0,
        }
    }

    pub(super) fn report_if_due(&mut self) {
        if self.last_report.elapsed() >= REPORT_INTERVAL {
            self.report(false);
            self.last_report = Instant::now();
        }
    }

    fn report(&self, stopped: bool) {
        tracing::info!(
            target: "conary_core::repository::catalog::parity::progress",
            schema_version = 1,
            walk_id = self.walk_id,
            profile = %self.manifest.profile,
            profile_revision_sha256 = %self.manifest.profile_revision_sha256,
            workers = self.workers,
            dispatched_roots = self.dispatched,
            completed_roots = self.completed.load(Ordering::Relaxed),
            emitted_roots = self.emitted,
            elapsed_seconds = self.started.elapsed().as_secs_f64(),
            stopped,
            "Resolution walk progress (diagnostic only)"
        );
    }
}

impl Drop for Progress<'_> {
    fn drop(&mut self) {
        self.report(true);
    }
}
