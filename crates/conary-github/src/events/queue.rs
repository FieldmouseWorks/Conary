//! A durable mailbox, with a single in-flight repository and its dirty bit.
use super::Kick;
use crate::flow::{
    Next, Report,
    persist::{self, Result},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct State {
    pending: BTreeSet<String>,
    active: Option<String>,
    #[serde(default)]
    status_events: BTreeMap<String, BTreeSet<u64>>,
    #[serde(default)]
    owned_statuses: BTreeMap<String, BTreeMap<u64, u64>>,
    #[serde(default)]
    failures: BTreeMap<String, BTreeMap<u64, u32>>,
    // One home for a PR's next unconsumed booking, replaced by each pass.
    waits: BTreeMap<String, BTreeMap<u64, u64>>,
}
#[derive(Debug, PartialEq, Eq)]
pub struct Work {
    pub repo: String,
}

pub struct Queue {
    state: Mutex<State>,
    changed: Condvar,
    path: PathBuf,
    _lock: persist::LocalLock,
}
impl Queue {
    pub fn open(data: &Path) -> Result<Self> {
        std::fs::create_dir_all(data)?;
        let lock = persist::LocalLock::take(&data.join("events.lock"))?;
        let path = data.join("events.json");
        let mut state: State = persist::load(&path)?;
        if let Some(repo) = state.active.take() {
            state.pending.insert(repo);
        }
        persist::save(&path, &state)?;
        Ok(Self {
            state: Mutex::new(state),
            changed: Condvar::new(),
            path,
            _lock: lock,
        })
    }
    #[cfg(test)]
    pub(crate) fn duplicate_lock(&self) -> std::fs::File {
        self._lock.duplicate()
    }
    fn change<T>(&self, f: impl FnOnce(&mut State) -> T) -> Result<T> {
        let mut state = self.state.lock().unwrap();
        let mut next = state.clone();
        let result = f(&mut next);
        persist::save(&self.path, &next)?;
        *state = next;
        self.changed.notify_one();
        Ok(result)
    }
    pub fn kick(&self, kick: Kick) -> Result<()> {
        // A pass reads repo-wide occupancy and train state even for one PR.
        // Coalescing to repo scope also handles checks with no PR association.
        self.change(|s| {
            s.pending.insert(kick.repo.to_lowercase());
        })
    }
    /// A signed status event remains distinct until the active writer has
    /// recorded its response ID. This also handles delivery before HTTP reply.
    pub fn status(&self, repo: &str, id: u64) -> Result<()> {
        self.change(|s| {
            s.status_events
                .entry(repo.to_lowercase())
                .or_default()
                .insert(id);
        })
    }
    pub fn own_status(&self, repo: &str, id: u64, now: u64) -> Result<()> {
        self.change(|s| {
            let owned = s.owned_statuses.entry(repo.to_lowercase()).or_default();
            owned.retain(|_, at| now < at.saturating_add(86400));
            owned.insert(id, now);
        })
    }
    /// Nonblocking form, also used by deterministic clock tests.
    pub fn take(&self, now: u64) -> Result<Option<Work>> {
        self.change(|s| {
            if s.active.is_some() {
                return None;
            }
            for (repo, ids) in std::mem::take(&mut s.status_events) {
                let owned = s.owned_statuses.get(&repo);
                if ids
                    .iter()
                    .any(|id| owned.is_none_or(|owned| !owned.contains_key(id)))
                {
                    s.pending.insert(repo);
                }
            }
            for (repo, waits) in &mut s.waits {
                let before = waits.len();
                waits.retain(|_, at| *at > now);
                if waits.len() != before {
                    s.pending.insert(repo.clone());
                }
            }
            let repo = s.pending.pop_first()?;
            s.active = Some(repo.clone());
            Some(Work { repo })
        })
    }
    pub fn finish(&self, work: &Work, reports: &[Report], now: u64) -> Result<()> {
        self.change(|s| {
            assert_eq!(s.active.as_deref(), Some(work.repo.as_str()));
            let waits = s.waits.entry(work.repo.clone()).or_default();
            waits.clear(); // A repo pass reports every open PR; closed PRs lose bookings.
            let failures = s.failures.entry(work.repo.clone()).or_default();
            failures.retain(|pr, _| reports.iter().any(|r| r.pr == *pr && r.error.is_some()));
            for report in reports {
                if report.error.is_some() {
                    let attempts = failures.entry(report.pr).or_default();
                    let retry = now.saturating_add(60 * (1u64 << (*attempts).min(6)));
                    *attempts = attempts.saturating_add(1);
                    waits.insert(report.pr, retry);
                }
                if let Some(at) = report.next.as_ref().and_then(|n| booking(n, now)) {
                    waits
                        .entry(report.pr)
                        .and_modify(|old| *old = (*old).min(at))
                        .or_insert(at);
                }
            }
            s.active = None;
            // Pending was populated independently while active; never clear it.
        })
    }
    /// Dispatch one mailbox entry through the supplied shared pass entry.
    /// A failed pass leaves its durable active entry for restart recovery.
    pub fn run_one(
        &self,
        now: u64,
        pass: impl FnOnce(&Work) -> Result<Vec<Report>>,
    ) -> Result<bool> {
        let Some(work) = self.take(now)? else {
            return Ok(false);
        };
        let reports = pass(&work)?;
        self.finish(&work, &reports, now)?;
        Ok(true)
    }
    /// Wait only for a message or a booked deadline. There is no polling clock.
    pub fn wait(&self) {
        let state = self.state.lock().unwrap();
        if !state.pending.is_empty() || !state.status_events.is_empty() {
            return;
        }
        let soonest = state.waits.values().flat_map(|w| w.values()).min().copied();
        match soonest {
            Some(at) => {
                drop(
                    self.changed
                        .wait_timeout(
                            state,
                            Duration::from_secs(at.saturating_sub(crate::flow::jobs::now())),
                        )
                        .unwrap(),
                );
            }
            None => {
                drop(self.changed.wait(state).unwrap());
            }
        }
    }
}
fn booking(next: &Next, now: u64) -> Option<u64> {
    // Already-consumed overdue deadlines must not form an immediate busy loop.
    next.wait_until
        .into_iter()
        .chain(next.overdue_at)
        .filter(|at| *at > now)
        .min()
}
