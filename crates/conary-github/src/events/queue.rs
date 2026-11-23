//! A durable mailbox: the repositories whose passes run, one pass at a time
//! each, and each one's dirty bit.
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
    // The repositories whose passes are running, one pass each. An earlier
    // conaryd's `active`, the one it ran, is read as no field, and the
    // `running` written here is no field to it: either one, starting, kicks
    // every repository it lists.
    #[serde(default)]
    running: BTreeSet<String>,
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
    /// Whether a pending repo may start now: not held, and no pass of it
    /// running.
    fn may_start(&self, repo: &str, held: &BTreeSet<String>) -> bool {
        !held.contains(repo) && !self.running.contains(repo)
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
        // Every pass that was running when the last conaryd stopped runs
        // again.
        let running = std::mem::take(&mut state.running);
        state.pending.extend(running);
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
        self.changed.notify_all();
        Ok(result)
    }
    pub fn kick(&self, kick: Kick) -> Result<()> {
        // A pass reads repo-wide occupancy and train state even for one PR.
        // Coalescing to repo scope also handles checks with no PR association.
        self.change(|s| {
            s.pending.insert(kick.repo.to_lowercase());
        })
    }
    /// A signed status event remains distinct until its repo's running pass
    /// has recorded its response ID. This also handles delivery before HTTP reply.
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
    /// Nonblocking form, also used by deterministic clock tests. It takes
    /// a pending repo with no pass running, while other repos' passes run.
    /// A running repo's status events, due bookings and recovery stay as
    /// they are until its pass ends, as they did while one pass ran at a
    /// time; its kicks set its dirty bit, and it runs again after.
    pub fn take(&self, now: u64) -> Result<Option<Work>> {
        self.change(|s| {
            let held = s.held(now);
            for (repo, ids) in std::mem::take(&mut s.status_events) {
                if s.running.contains(&repo) {
                    s.status_events.insert(repo, ids); // Its response IDs are still coming.
                    continue;
                }
                let owned = s.owned_statuses.get(&repo);
                if ids
                    .iter()
                    .any(|id| owned.is_none_or(|owned| !owned.contains_key(id)))
                {
                    s.pending.insert(repo);
                }
            }
            for (repo, waits) in &mut s.waits {
                if held.contains(repo) || s.running.contains(repo) {
                    continue; // Its bookings stay booked until it may run.
                }
                let before = waits.len();
                waits.retain(|_, at| *at > now);
                if waits.len() != before {
                    s.pending.insert(repo.clone());
                }
            }
            for (repo, recovery) in &s.recovery {
                if recovery.at <= now && !s.running.contains(repo) {
                    s.pending.insert(repo.clone());
                }
            }
            let repo = s.pending.iter().find(|r| s.may_start(r, &held)).cloned()?;
            s.pending.remove(&repo);
            s.running.insert(repo.clone());
            Some(Work { repo })
        })
    }
    pub fn finish(&self, work: &Work, reports: &[Report], now: u64) -> Result<()> {
        self.change(|s| {
            assert!(s.running.remove(&work.repo), "{} wasn't running", work.repo);
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
            // Pending was populated independently while active; never clear it.
        })
    }
    /// Books a recovery for a repo whose whole pass failed, before any
    /// report, its backoff counted from `now`: the time the pass failed. Its PR bookings stay as the last finished pass left them, and
    /// kicks queued meanwhile stay queued; both wait for the recovery.
    pub fn fail(&self, work: &Work, now: u64) -> Result<Recovery> {
        self.change(|s| {
            assert!(s.running.remove(&work.repo), "{} wasn't running", work.repo);
            let recovery = s.recovery.entry(work.repo.clone()).or_insert(Recovery {
                at: now,
                attempts: 0,
            });
            recovery.at = backoff(now, recovery.attempts);
            recovery.attempts = recovery.attempts.saturating_add(1);
            *recovery
        })
    }
    /// A repo's booked recovery, if its last pass failed.
    pub fn recovery(&self, repo: &str) -> Option<Recovery> {
        self.state.lock().unwrap().recovery.get(repo).copied()
    }
    /// Dispatch one mailbox entry through the supplied shared pass entry.
    /// A failed pass books its repo's recovery; only the mailbox's own
    /// errors are returned. `clock` is read before the pass, to take its
    /// work and judge which of its bookings it already saw, and again after
    /// a failed one, so its whole backoff starts once the pass has given up,
    /// however long its requests took.
    pub fn run_one(
        &self,
        clock: impl Fn() -> u64,
        pass: impl FnOnce(&Work) -> Result<Vec<Report>>,
    ) -> Result<Ran> {
        let started = clock();
        let Some(work) = self.take(started)? else {
            return Ok(Ran::Idle);
        };
        self.settle(work, started, clock, pass)
    }
    /// Runs `work`'s pass, taken at `started`, and books what it came to.
    fn settle(
        &self,
        work: Work,
        started: u64,
        clock: impl Fn() -> u64,
        pass: impl FnOnce(&Work) -> Result<Vec<Report>>,
    ) -> Result<Ran> {
        match pass(&work) {
            Ok(reports) => {
                self.finish(&work, &reports, started)?;
                Ok(Ran::Finished)
            }
            Err(_) => {
                let recovery = self.fail(&work, clock())?;
                Ok(Ran::Failed {
                    repo: work.repo,
                    recovery,
                })
            }
        }
    }
    /// Runs every pass as it comes due, for as long as the mailbox can be
    /// written: a failed pass is booked, never fatal. Each repo's pass runs
    /// on a thread of its own as soon as it's due, whatever other repos'
    /// passes are doing, and a repo's next pass waits for its last.
    /// `failed` hears of each failed pass, for the log. A mailbox that
    /// can't be written stops the worker once the passes running end.
    pub fn work(
        &self,
        pass: impl Fn(&Work) -> Result<Vec<Report>> + Sync,
        failed: impl Fn(&str, Recovery) + Sync,
    ) -> Result<std::convert::Infallible> {
        let broken: Mutex<Option<persist::Error>> = Mutex::new(None);
        let (pass, failed, broken) = (&pass, &failed, &broken);
        std::thread::scope(|scope| {
            loop {
                if let Some(e) = broken.lock().unwrap().take() {
                    return Err(e);
                }
                let started = crate::flow::jobs::now();
                let Some(work) = self.take(started)? else {
                    self.sleep(|| broken.lock().unwrap().is_some());
                    continue;
                };
                scope.spawn(move || {
                    match self.settle(work, started, crate::flow::jobs::now, pass) {
                        Ok(Ran::Failed { repo, recovery }) => failed(&repo, recovery),
                        Ok(_) => {}
                        Err(e) => {
                            *broken.lock().unwrap() = Some(e);
                            // Under the state's lock, so the sleeper either
                            // sees it or is already waiting to be woken.
                            let _state = self.state.lock().unwrap();
                            self.changed.notify_all();
                        }
                    }
                });
            }
        })
    }
    /// Wait only for a message or a booked deadline. There is no polling clock.
    pub fn wait(&self) {
        self.sleep(|| false);
    }
    /// [`Self::wait`], or not at all once `stop` says so. `stop` is asked
    /// under the state's lock.
    fn sleep(&self, stop: impl Fn() -> bool) {
        let state = self.state.lock().unwrap();
        if stop() {
            return;
        }
        let now = crate::flow::jobs::now();
        let held = state.held(now);
        // A running repo's kicks, status events, bookings and recovery wake
        // nothing before its pass ends, which wakes the sleeper itself.
        if state.pending.iter().any(|r| state.may_start(r, &held))
            || state
                .status_events
                .keys()
                .any(|r| !state.running.contains(r))
        {
            return;
        }
        // A held repo's own bookings wake nothing before its recovery does.
        let soonest = state
            .waits
            .iter()
            .filter(|(repo, _)| state.may_start(repo, &held))
            .flat_map(|(_, w)| w.values().copied())
            .chain(
                state
                    .recovery
                    .iter()
                    .filter(|(repo, _)| !state.running.contains(*repo))
                    .map(|(_, r)| r.at),
            )
            .min();
        match soonest {
            Some(at) => {
                drop(
                    self.changed
                        .wait_timeout(state, Duration::from_secs(at.saturating_sub(now)))
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
