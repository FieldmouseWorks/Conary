//! Checks too heavy to run on every push. A repository's machine
//! configuration names them; they start only once review is ready, and
//! landing needs their successful evidence. Nothing here is on by default.
//!
//! Review readiness and landing permission are different questions. Starting
//! the work asks only the first (a clean review with its threads resolved, or
//! no reviewer configured); the `review-settled` permission asks the second,
//! so waiting for the work's evidence can never be what blocks its start.
use super::decide::{Decision, at, wait};
use super::persist::Result;
use super::*;
use crate::attest::Actions;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// How one deferred check is started: a `workflow_dispatch` workflow of the
/// repository, run from `ref` (the default branch if omitted). The workflow
/// gets the inputs `pr`, `head`, `base`, `check` and `nonce`; see
/// docs/flow.md for what it must do with them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Start {
    pub workflow: String,
    #[serde(rename = "ref", default)]
    pub git_ref: Option<String>,
}

/// Machine-owned. No checks means off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DeferredConfig {
    /// Check name -> how to start it.
    pub checks: BTreeMap<String, Start>,
    /// Seconds a dispatch may stay unseen (or a finished run without its
    /// evidence) before it counts as lost and may be sent again.
    pub start_timeout: u64,
    /// Seconds a started run may take before it is cancelled and lost.
    pub run_timeout: u64,
    /// Dispatches per check and head before giving up.
    pub attempts: u32,
}

impl Default for DeferredConfig {
    fn default() -> Self {
        Self {
            checks: BTreeMap::new(),
            start_timeout: 900,
            run_timeout: 21600,
            attempts: 3,
        }
    }
}

impl DeferredConfig {
    pub fn validate(&self, settings: &Settings, negative: Option<&str>) -> Result<()> {
        for (name, start) in &self.checks {
            if name.trim().is_empty() || start.workflow.trim().is_empty() {
                return Err("a deferred check needs a name and a workflow".into());
            }
            if start.workflow.contains(['/', '?', '#', ' ']) {
                return Err(format!(
                    "deferred workflow {:?} must be a file name or id",
                    start.workflow
                )
                .into());
            }
            if start
                .git_ref
                .as_deref()
                .is_some_and(|r| r.trim().is_empty())
            {
                return Err("a deferred check's ref cannot be empty".into());
            }
            if settings.proofs.keys().any(|p| p.eq_ignore_ascii_case(name))
                || negative == Some(name.as_str())
                || name.starts_with("conary/")
                || name == super::SETTLED
            {
                return Err(format!("deferred check {name:?} collides with another gate").into());
            }
        }
        if !self.checks.is_empty() && (self.attempts == 0 || self.start_timeout == 0) {
            return Err("deferred attempts and start_timeout must be above zero".into());
        }
        Ok(())
    }
}

/// Seconds between asking again to stop a run that is still going.
const RETRY: u64 = 60;

/// A dispatch Conary owns, saved before it is sent so that an uncertain
/// response is never answered by sending it again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dispatch {
    pub check: String,
    /// The workflow it was sent to, kept so that changing the configuration
    /// never loses a run that is still going.
    pub workflow: String,
    /// The ref it was sent to, resolved: the configured one or the default
    /// branch then. A different ref is different work.
    pub git_ref: String,
    pub head: String,
    /// Unique to this dispatch. The run is titled with it and the signed
    /// evidence of the job names it.
    pub nonce: String,
    pub sent_at: u64,
    /// The workflow run, once seen.
    pub run: Option<u64>,
    /// When GitHub last accepted a request to stop it. A request is not a
    /// stop: the dispatch is kept until its run is seen finished.
    pub cancel_asked: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deferral {
    /// Nothing live is owned for this head: start it.
    Unstarted,
    /// Owned and neither lost nor finished; look again at `wake`.
    InFlight { wake: u64 },
    /// Earlier work of this check (an old head, a lost run) may still be
    /// running: nothing replaces it until it is seen stopped.
    Stopping { wake: u64 },
    /// A successful check run of the owned run's own job. `run` and
    /// `check_run` are the one record that must also carry the attestation.
    Done { run: u64, check_run: u64 },
    /// Lost too many times for this head.
    Exhausted,
}

/// The evidence a deferred check offers: one owned dispatch and the check run
/// of its own job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Proof {
    pub name: String,
    pub nonce: String,
    pub run: u64,
    /// The commit the run is on, which hosts its job's check run.
    pub sha: String,
    pub check_run: u64,
}

/// What reading GitHub says about the dispatches in a record.
#[derive(Debug, Default)]
pub(super) struct Observation {
    pub deferrals: BTreeMap<String, Deferral>,
    /// Nonces of runs to stop: obsolete heads, or lost and still running.
    pub stale: Vec<String>,
    /// The earliest moment a dispatch needs another look, whatever else the
    /// pass decides, from the same observed state that decides loss.
    pub wake: Option<u64>,
    /// The record behind each `Done`.
    pub proofs: BTreeMap<String, Proof>,
    adopt: Vec<(String, u64)>,
    retire: BTreeSet<String>,
}

impl Observation {
    /// Save what was learned. Never done in a dry run.
    pub(super) fn apply(&self, record: &mut Record) -> bool {
        let before = record.dispatches.clone();
        for d in &mut record.dispatches {
            if let Some((_, run)) = self.adopt.iter().find(|(n, _)| *n == d.nonce) {
                d.run = Some(*run);
            }
        }
        record
            .dispatches
            .retain(|d| !self.retire.contains(&d.nonce));
        record.dispatches != before
    }
}

fn titled(run: &crate::api::WorkflowRun, nonce: &str) -> bool {
    run.display_title
        .as_deref()
        .is_some_and(|t| t.split_whitespace().any(|w| w == nonce))
}

fn lapses(d: &Dispatch, run: Option<&crate::api::WorkflowRun>, c: &DeferredConfig) -> Option<u64> {
    // When it will count as lost, and None when it already has.
    Some(match run {
        Some(r) if r.status == "completed" && r.conclusion.as_deref() != Some("success") => {
            return None;
        }
        Some(r) if r.status != "completed" => d.sent_at.saturating_add(c.run_timeout),
        _ => d.sent_at.saturating_add(c.start_timeout),
    })
}

/// Read the dispatches and their evidence. Evidence is a completed, successful
/// check run that is the job of the owned dispatch's own workflow run, on the
/// commit the run is on (a dispatch's is its ref's tip, never the PR's head),
/// that `prove` also accepts as attested for this head, tree and nonce. The
/// name alone is nothing, a missing run is never a pass, and a candidate
/// `prove` refuses leaves the dispatch subject to loss and retry.
pub(super) fn observe<G: Read + Actions + ?Sized>(
    github: &G,
    f: &Facts,
    now: u64,
    prove: &dyn Fn(&Proof) -> bool,
) -> std::result::Result<Observation, crate::api::ApiError> {
    let config = &f.settings.deferred;
    let mut seen = Observation::default();
    if config.checks.is_empty() && f.dispatches.is_empty() {
        return Ok(seen);
    }
    let repo = f.repo.to_string();
    // A saved run is read by its id; one not yet seen is looked for in its own
    // workflow's listing from when the earliest such dispatch was sent.
    let mut listed: BTreeMap<&str, Vec<crate::api::WorkflowRun>> = BTreeMap::new();
    for d in f.dispatches.iter().filter(|d| d.run.is_none()) {
        if !listed.contains_key(d.workflow.as_str()) {
            let since = f
                .dispatches
                .iter()
                .filter(|o| o.run.is_none() && o.workflow == d.workflow)
                .map(|o| o.sent_at)
                .min()
                .unwrap_or(d.sent_at)
                .saturating_sub(300);
            listed.insert(
                &d.workflow,
                github.workflow_runs(&repo, &d.workflow, since)?,
            );
        }
    }
    let mut run_of: BTreeMap<&str, Option<crate::api::WorkflowRun>> = BTreeMap::new();
    for d in &f.dispatches {
        let run = match d.run {
            Some(id) => github.workflow_run(&repo, id)?,
            None => listed
                .get(d.workflow.as_str())
                .and_then(|l| l.iter().find(|r| titled(r, &d.nonce)))
                .cloned(),
        };
        run_of.insert(&d.nonce, run);
    }
    let obsolete = |d: &Dispatch| {
        d.head != f.revision.head
            || config.checks.get(&d.check).is_none_or(|s| {
                s.workflow != d.workflow
                    || s.git_ref.as_deref().unwrap_or(&f.default_branch) != d.git_ref
            })
    };
    let mut unsettled = Vec::new();
    let mut asks: Vec<(Option<u64>, String)> = Vec::new();
    for d in &f.dispatches {
        let run = run_of[d.nonce.as_str()].as_ref();
        if d.run.is_none()
            && let Some(r) = run
        {
            seen.adopt.push((d.nonce.clone(), r.id));
        }
        let lost = lapses(d, run, config).is_none_or(|t| now >= t);
        let live = run.is_some_and(|r| r.status != "completed");
        let settled = !live && (run.is_some() || d.run.is_some() || lost);
        if live && (obsolete(d) || lost) && d.cancel_asked.is_none_or(|t| now >= t + RETRY) {
            asks.push((d.cancel_asked, d.nonce.clone()));
        }
        // Forgotten only once the run is seen finished (or was never there).
        if obsolete(d) && settled {
            seen.retire.insert(d.nonce.clone());
        } else if !settled && (obsolete(d) || lost) {
            unsettled.push(d);
        }
    }
    // Never-asked runs first, then the longest since asked: an ignored
    // request never starves another run's first.
    asks.sort();
    seen.stale = asks.into_iter().map(|(_, n)| n).collect();
    let here: Vec<&Dispatch> = f
        .dispatches
        .iter()
        .filter(|d| !obsolete(d) && !seen.retire.contains(&d.nonce))
        .collect();
    for name in config.checks.keys() {
        let owned: Vec<&&Dispatch> = here.iter().filter(|d| &d.check == name).collect();
        let mut proof = None;
        for d in &owned {
            let Some(run) = run_of[d.nonce.as_str()].as_ref() else {
                continue;
            };
            if run.status != "completed" || run.conclusion.as_deref() != Some("success") {
                continue;
            }
            for c in github.check_runs(&repo, &run.head_sha)?.iter().filter(|c| {
                &c.name == name
                    && c.status == "completed"
                    && c.conclusion.as_deref() == Some("success")
            }) {
                if github.job(&repo, c.id)?.is_some_and(|j| j.run_id == run.id) {
                    let candidate = Proof {
                        name: name.clone(),
                        nonce: d.nonce.clone(),
                        run: run.id,
                        sha: run.head_sha.clone(),
                        check_run: c.id,
                    };
                    if prove(&candidate) {
                        proof = Some(candidate);
                    }
                }
            }
        }
        let stopping: Vec<u64> = unsettled
            .iter()
            .filter(|d| &d.check == name)
            .map(|d| {
                d.cancel_asked
                    .map_or(now.saturating_add(RETRY), |t| t.saturating_add(RETRY))
                    .max(now.saturating_add(1))
            })
            .collect();
        let state = if let Some(done) = proof {
            let state = Deferral::Done {
                run: done.run,
                check_run: done.check_run,
            };
            seen.proofs.insert(name.clone(), done);
            state
        } else if let Some(wake) = stopping.into_iter().min() {
            Deferral::Stopping { wake }
        } else {
            let wakes: Vec<u64> = owned
                .iter()
                .filter_map(|d| lapses(d, run_of[d.nonce.as_str()].as_ref(), config))
                .filter(|t| now < *t)
                .collect();
            match wakes.into_iter().min() {
                Some(wake) => Deferral::InFlight { wake },
                None if owned.len() >= config.attempts as usize => Deferral::Exhausted,
                None => Deferral::Unstarted,
            }
        };
        seen.deferrals.insert(name.clone(), state);
    }
    // Booked from the same states: unseen, running, finished awaiting its
    // evidence, and stopping, whatever the pass then decides.
    let proven = |d: &Dispatch| {
        seen.deferrals.values().any(|s| {
            matches!(s, Deferral::Done { run, .. }
                if Some(*run) == run_of[d.nonce.as_str()].as_ref().map(|r| r.id))
        })
    };
    seen.wake = f
        .dispatches
        .iter()
        .filter(|d| !seen.retire.contains(&d.nonce) && !proven(d))
        .flat_map(|d| {
            [
                lapses(d, run_of[d.nonce.as_str()].as_ref(), config),
                d.cancel_asked.map(|t| t.saturating_add(RETRY)),
            ]
        })
        .flatten()
        .chain((seen.stale.len() > 1).then(|| now.saturating_add(1)))
        .filter(|t| *t > now)
        .min();
    Ok(seen)
}

/// The moment to look again: what was observed, and what this pass itself
/// changed in the record (a dispatch just sent, a stop just asked).
pub(super) fn deadline(f: &Facts, dispatches: &[Dispatch], now: u64) -> Option<u64> {
    let start = f.settings.deferred.start_timeout;
    dispatches
        .iter()
        .filter(|d| {
            !f.dispatches
                .iter()
                .any(|o| o.nonce == d.nonce && o.cancel_asked == d.cancel_asked)
        })
        .flat_map(|d| {
            [
                Some(d.sent_at.saturating_add(start)),
                d.cancel_asked.map(|t| t.saturating_add(RETRY)),
            ]
        })
        .flatten()
        .chain(f.deferred_wake)
        .filter(|t| *t > now)
        .min()
}

fn names(f: &Facts, wanted: impl Fn(&Deferral) -> bool) -> Vec<String> {
    f.deferred
        .iter()
        .filter(|(_, d)| wanted(d))
        .map(|(n, _)| n.clone())
        .collect()
}

/// Review is ready: start whatever is not started. The caller has already
/// sent red checks and failed judgments to a fix.
pub(super) fn start(f: &Facts) -> Option<Decision> {
    let unstarted = names(f, |d| *d == Deferral::Unstarted);
    (!unstarted.is_empty()).then(|| {
        at(
            Stage::Ci,
            Step::StartDeferred(unstarted),
            "Review is ready: starting the deferred checks",
        )
    })
}

/// No reviewer is configured, so nothing will ever make review "ready" in the
/// ordinary way; the deferred checks start when nothing else is open.
pub(super) fn without_reviewer(f: &Facts, red: bool, decision: &Decision) -> Option<Decision> {
    (f.settings.reviewers.is_empty()
        && f.open_threads == 0
        && !f.changes_requested
        && !red
        && matches!(decision.1, Step::Wait { .. })
        && matches!(decision.0, Stage::Review | Stage::Reviewer))
    .then(|| start(f))
    .flatten()
}

/// Landing waits for evidence from every deferred check.
pub(super) fn gate(f: &Facts) -> Option<Decision> {
    let missing: Vec<_> = f
        .deferred
        .iter()
        .filter(|(_, d)| !matches!(d, Deferral::Done { .. }))
        .collect();
    if missing.is_empty() {
        return None;
    }
    let wake = missing
        .iter()
        .filter_map(|(_, d)| match d {
            Deferral::InFlight { wake } | Deferral::Stopping { wake } => Some(*wake),
            _ => None,
        })
        .min();
    let list = missing
        .iter()
        .map(|(n, _)| n.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let why = if missing.iter().any(|(_, d)| **d == Deferral::Exhausted) {
        format!("Deferred checks lost too many times: {list}")
    } else {
        format!("Waiting for deferred checks: {list}")
    };
    Some(wait(Stage::Ci, wake, &why))
}

/// Every configured check's evidence, read afresh: the owned run and the
/// check run of its own job, which the caller must then see attested.
pub(crate) fn verify<G: Read + Actions + ?Sized>(
    github: &G,
    f: &Facts,
    now: u64,
    prove: &dyn Fn(&Proof) -> bool,
) -> Result<Vec<Proof>> {
    let seen = observe(github, f, now, prove)?;
    f.settings
        .deferred
        .checks
        .keys()
        .map(|name| {
            seen.proofs
                .get(name)
                .cloned()
                .ok_or_else(|| format!("deferred check {name:?} has no successful evidence").into())
        })
        .collect()
}

fn input(f: &Facts, check: &str, nonce: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("pr".into(), f.number.to_string()),
        ("head".into(), f.revision.head.clone()),
        ("base".into(), f.revision.base.clone()),
        ("check".into(), check.into()),
        ("nonce".into(), nonce.into()),
    ])
}

/// Send the dispatches. Each is saved before it is sent, and a send that fails
/// leaves it saved: the response may have been lost after GitHub accepted it,
/// so it is looked for by nonce and only sent again once it has lapsed.
pub(super) fn begin<G: Write>(
    pass: &Pass<'_>,
    github: &G,
    f: &Facts,
    wanted: &[String],
    record: &mut Record,
    data: &Data,
) -> Result<()> {
    let config = &f.settings.deferred;
    for name in wanted {
        let start = config
            .checks
            .get(name)
            .ok_or("deferred check is not configured")?;
        let mine = |d: &&Dispatch| &d.check == name && d.head == f.revision.head;
        let sent = record.dispatches.iter().filter(mine).count();
        if record.dispatches.iter().filter(mine).any(|d| {
            d.cancel_asked.is_none() && d.sent_at.saturating_add(config.start_timeout) > pass.now
        }) || sent >= config.attempts as usize
        {
            continue;
        }
        record.sequence = record
            .sequence
            .checked_add(1)
            .ok_or("dispatch sequence exhausted")?;
        let nonce = format!(
            "{}-{}-{}",
            f.number,
            record.sequence,
            f.revision.head.get(..12).unwrap_or(&f.revision.head)
        );
        record.dispatches.push(Dispatch {
            check: name.clone(),
            workflow: start.workflow.clone(),
            git_ref: start
                .git_ref
                .clone()
                .unwrap_or_else(|| f.default_branch.clone()),
            head: f.revision.head.clone(),
            nonce: nonce.clone(),
            sent_at: pass.now,
            run: None,
            cancel_asked: None,
        });
        data.save(f.number, record)?;
        github.dispatch(
            &f.repo.to_string(),
            &start.workflow,
            &record.dispatches.last().expect("just saved").git_ref,
            &input(f, name, &nonce),
        )?;
    }
    Ok(())
}

/// Ask GitHub to stop one run and remember when. Asking is not stopping: the
/// dispatch stays until its run is seen finished, and is asked again.
pub(super) fn cancel<G: Write>(
    github: &G,
    f: &Facts,
    nonce: &str,
    now: u64,
    record: &mut Record,
    data: &Data,
) -> Result<()> {
    let d = record
        .dispatches
        .iter_mut()
        .find(|d| d.nonce == nonce)
        .ok_or("dispatch missing")?;
    let run = d.run.ok_or("run not found")?;
    github.cancel_run(&f.repo.to_string(), run)?;
    d.cancel_asked = Some(now);
    data.save(f.number, record)?;
    Ok(())
}
