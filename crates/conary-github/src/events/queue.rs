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
    // A repo whose whole pass failed, until a pass of it finishes.
    #[serde(default)]
    recovery: BTreeMap<String, Recovery>,
}
impl State {
    /// Repos whose failed pass holds them, kicks and bookings alike, until
    /// their recovery time.
    fn held(&self, now: u64) -> BTreeSet<String> {
        self.recovery
            .iter()
            .filter(|(_, r)| r.at > now)
            .map(|(repo, _)| repo.clone())
            .collect()
    }
}
#[derive(Debug, PartialEq, Eq)]
pub struct Work {
    pub repo: String,
}
/// A repository's booked retry after its whole pass failed: when it runs
/// again, and how many of its passes in a row have failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recovery {
    pub at: u64,
    pub attempts: u32,
}
/// What [`Queue::run_one`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Ran {
    /// Nothing was due.
    Idle,
    /// A pass ran and its reports were booked.
    Finished,
    /// A whole pass failed, and its repository's recovery is booked.
    Failed { repo: String, recovery: Recovery },
}
/// The retry after `attempts` failures in a row: 60 seconds, doubling up to
/// 64 minutes.
fn backoff(now: u64, attempts: u32) -> u64 {
    now.saturating_add(60 * (1u64 << attempts.min(6)))
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
            let held = s.held(now);
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
                if held.contains(repo) {
                    continue; // Its bookings stay booked until it may run.
                }
                let before = waits.len();
                waits.retain(|_, at| *at > now);
                if waits.len() != before {
                    s.pending.insert(repo.clone());
                }
            }
            for (repo, recovery) in &s.recovery {
                if recovery.at <= now {
                    s.pending.insert(repo.clone());
                }
            }
            let repo = s.pending.iter().find(|r| !held.contains(*r)).cloned()?;
            s.pending.remove(&repo);
            s.active = Some(repo.clone());
            Some(Work { repo })
        })
    }
    pub fn finish(&self, work: &Work, reports: &[Report], now: u64) -> Result<()> {
        self.change(|s| {
            assert_eq!(s.active.as_deref(), Some(work.repo.as_str()));
            s.recovery.remove(&work.repo);
            let waits = s.waits.entry(work.repo.clone()).or_default();
            waits.clear(); // A repo pass reports every open PR; closed PRs lose bookings.
            let failures = s.failures.entry(work.repo.clone()).or_default();
            failures.retain(|pr, _| reports.iter().any(|r| r.pr == *pr && r.error.is_some()));
            for report in reports {
                if report.error.is_some() {
                    let attempts = failures.entry(report.pr).or_default();
                    let retry = backoff(now, *attempts);
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
    /// Books a recovery for a repo whose whole pass failed, before any
    /// report. Its PR bookings stay as the last finished pass left them, and
    /// kicks queued meanwhile stay queued; both wait for the recovery.
    pub fn fail(&self, work: &Work, now: u64) -> Result<Recovery> {
        self.change(|s| {
            assert_eq!(s.active.as_deref(), Some(work.repo.as_str()));
            let recovery = s.recovery.entry(work.repo.clone()).or_insert(Recovery {
                at: now,
                attempts: 0,
            });
            recovery.at = backoff(now, recovery.attempts);
            recovery.attempts = recovery.attempts.saturating_add(1);
            s.active = None;
            *recovery
        })
    }
    /// A repo's booked recovery, if its last pass failed.
    pub fn recovery(&self, repo: &str) -> Option<Recovery> {
        self.state.lock().unwrap().recovery.get(repo).copied()
    }
    /// Dispatch one mailbox entry through the supplied shared pass entry.
    /// A failed pass books its repo's recovery; only the mailbox's own
    /// errors are returned.
    pub fn run_one(
        &self,
        now: u64,
        pass: impl FnOnce(&Work) -> Result<Vec<Report>>,
    ) -> Result<Ran> {
        let Some(work) = self.take(now)? else {
            return Ok(Ran::Idle);
        };
        match pass(&work) {
            Ok(reports) => {
                self.finish(&work, &reports, now)?;
                Ok(Ran::Finished)
            }
            Err(_) => {
                let recovery = self.fail(&work, now)?;
                Ok(Ran::Failed {
                    repo: work.repo,
                    recovery,
                })
            }
        }
    }
    /// Runs every pass as it comes due, for as long as the mailbox can be
    /// written: a failed pass is booked, never fatal. `failed` hears of each
    /// failed pass, for the log.
    pub fn work(
        &self,
        mut pass: impl FnMut(&Work) -> Result<Vec<Report>>,
        mut failed: impl FnMut(&str, Recovery),
    ) -> Result<std::convert::Infallible> {
        loop {
            match self.run_one(crate::flow::jobs::now(), &mut pass)? {
                Ran::Idle => self.wait(),
                Ran::Finished => {}
                Ran::Failed { repo, recovery } => failed(&repo, recovery),
            }
        }
    }
    /// Wait only for a message or a booked deadline. There is no polling clock.
    pub fn wait(&self) {
        let state = self.state.lock().unwrap();
        let held = state.held(crate::flow::jobs::now());
        if state.pending.iter().any(|r| !held.contains(r)) || !state.status_events.is_empty() {
            return;
        }
        // A held repo's own bookings wake nothing before its recovery does.
        let soonest = state
            .waits
            .iter()
            .filter(|(repo, _)| !held.contains(*repo))
            .flat_map(|(_, w)| w.values().copied())
            .chain(state.recovery.values().map(|r| r.at))
            .min();
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
