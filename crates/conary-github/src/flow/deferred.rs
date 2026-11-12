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
    pub check_run: u64,
}

/// What reading GitHub says about the dispatches in a record.
#[derive(Debug, Default)]
pub(super) struct Observation {
    pub deferrals: BTreeMap<String, Deferral>,
    /// Nonces of runs to stop: obsolete heads, or lost and still running.
    pub stale: Vec<String>,
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
/// check run on the head that is the job of the owned dispatch's own workflow
/// run: the name alone is nothing, and a missing run is never a pass.
pub(super) fn observe<G: Read + Actions + ?Sized>(
    github: &G,
    f: &Facts,
    now: u64,
) -> std::result::Result<Observation, crate::api::ApiError> {
    let config = &f.settings.deferred;
    let mut seen = Observation::default();
    if config.checks.is_empty() && f.dispatches.is_empty() {
        return Ok(seen);
    }
    let repo = f.repo.to_string();
    // A saved run is read by its id; one not yet seen is looked for in the
    // whole listing of its own workflow, as it was sent.
    let mut listed: BTreeMap<&str, Vec<crate::api::WorkflowRun>> = BTreeMap::new();
    for d in f.dispatches.iter().filter(|d| d.run.is_none()) {
        if !listed.contains_key(d.workflow.as_str()) {
            listed.insert(&d.workflow, github.workflow_runs(&repo, &d.workflow)?);
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
            || config.checks.get(&d.check).map(|s| s.workflow.as_str()) != Some(d.workflow.as_str())
    };
    let mut unsettled = Vec::new();
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
            seen.stale.push(d.nonce.clone());
        }
        // Forgotten only once the run is seen finished (or was never there).
        if obsolete(d) && settled {
            seen.retire.insert(d.nonce.clone());
        } else if !settled && (obsolete(d) || lost) {
            unsettled.push(d);
        }
    }
    let here: Vec<&Dispatch> = f
        .dispatches
        .iter()
        .filter(|d| !obsolete(d) && !seen.retire.contains(&d.nonce))
        .collect();
    let checks = if here.is_empty() {
        Vec::new()
    } else {
        github.check_runs(&repo, &f.revision.head)?
    };
    for name in config.checks.keys() {
        let owned: Vec<&&Dispatch> = here.iter().filter(|d| &d.check == name).collect();
        let mut proof = None;
        for d in &owned {
            let Some(run) = d
                .run
                .or_else(|| run_of[d.nonce.as_str()].as_ref().map(|r| r.id))
            else {
                continue;
            };
            let finished = run_of[d.nonce.as_str()].as_ref().is_some_and(|r| {
                r.status == "completed" && r.conclusion.as_deref() == Some("success")
            });
            if !finished {
                continue;
            }
            for c in checks.iter().filter(|c| {
                &c.name == name
                    && c.status == "completed"
                    && c.conclusion.as_deref() == Some("success")
            }) {
                if github.job(&repo, c.id)?.is_some_and(|j| j.run_id == run) {
                    proof = Some(Deferral::Done {
                        run,
                        check_run: c.id,
                    });
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
            done
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
    Ok(seen)
}

/// The earliest moment a dispatch in the record needs another look, whatever
/// else the pass decided: its start or run deadline, or the next ask to stop
/// it. Work whose evidence is in needs none. Moments already past are not
/// booked: the pass that saw them acted on them.
pub(super) fn deadline(f: &Facts, dispatches: &[Dispatch], now: u64) -> Option<u64> {
    let config = &f.settings.deferred;
    dispatches
        .iter()
        .filter(|d| {
            !f.deferred
                .values()
                .any(|s| matches!(s, Deferral::Done { run, .. } if Some(*run) == d.run))
        })
        .flat_map(|d| {
            let due = if d.run.is_some() {
                config.run_timeout
            } else {
                config.start_timeout
            };
            [
                Some(d.sent_at.saturating_add(due)),
                d.cancel_asked.map(|t| t.saturating_add(RETRY)),
            ]
        })
        .flatten()
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
) -> Result<Vec<Proof>> {
    let seen = observe(github, f, now)?;
    let mut proofs = Vec::new();
    for name in f.settings.deferred.checks.keys() {
        let Some(Deferral::Done { run, check_run }) = seen.deferrals.get(name) else {
            return Err(format!("deferred check {name:?} has no successful evidence").into());
        };
        let nonce = f
            .dispatches
            .iter()
            .find(|d| d.run == Some(*run))
            .map(|d| d.nonce.clone())
            .or_else(|| {
                seen.adopt
                    .iter()
                    .find(|(_, r)| r == run)
                    .map(|(n, _)| n.clone())
            })
            .ok_or("deferred evidence has no owned dispatch")?;
        proofs.push(Proof {
            name: name.clone(),
            nonce,
            run: *run,
            check_run: *check_run,
        });
    }
    Ok(proofs)
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
            start.git_ref.as_deref().unwrap_or(&f.default_branch),
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
