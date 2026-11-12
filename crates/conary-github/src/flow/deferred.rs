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

/// A dispatch Conary owns, saved before it is sent so that an uncertain
/// response is never answered by sending it again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dispatch {
    pub check: String,
    pub head: String,
    /// Unique to this dispatch. The run is titled with it and the check run it
    /// reports carries it as its external ID.
    pub nonce: String,
    pub sent_at: u64,
    /// The workflow run, once seen.
    pub run: Option<u64>,
    /// Asked to stop, or given up on.
    pub cancelled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deferral {
    /// Nothing live is owned for this head: start it.
    Unstarted,
    /// Owned and neither lost nor finished; look again at `wake`.
    InFlight { wake: u64 },
    /// A successful check run carrying an owned dispatch's nonce.
    Done,
    /// Lost too many times for this head.
    Exhausted,
}

/// What reading GitHub says about the dispatches in a record.
#[derive(Debug, Default)]
pub(super) struct Observation {
    pub deferrals: BTreeMap<String, Deferral>,
    /// Nonces of runs to cancel: obsolete heads, or lost and still running.
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
/// check run on the head carrying the nonce of a dispatch this record owns:
/// the name alone is nothing, and a missing run is never a pass.
pub(super) fn observe<G: Read + ?Sized>(
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
    let mut runs: BTreeMap<&str, Vec<crate::api::WorkflowRun>> = BTreeMap::new();
    let workflows: BTreeSet<&str> = f
        .dispatches
        .iter()
        .filter_map(|d| config.checks.get(&d.check))
        .map(|s| s.workflow.as_str())
        .collect();
    for workflow in workflows {
        runs.insert(workflow, github.workflow_runs(&repo, workflow)?);
    }
    let run_of = |d: &Dispatch| -> Option<&crate::api::WorkflowRun> {
        let list = runs.get(config.checks.get(&d.check)?.workflow.as_str())?;
        list.iter()
            .find(|r| Some(r.id) == d.run)
            .or_else(|| list.iter().find(|r| titled(r, &d.nonce)))
    };
    for d in &f.dispatches {
        let run = run_of(d);
        if d.run.is_none()
            && let Some(r) = run
        {
            seen.adopt.push((d.nonce.clone(), r.id));
        }
        let obsolete = d.head != f.revision.head;
        let lost = lapses(d, run, config).is_none_or(|t| now >= t);
        let live = run.is_some_and(|r| r.status != "completed");
        if live && !d.cancelled && (obsolete || lost) {
            seen.stale.push(d.nonce.clone());
        }
        // An obsolete dispatch is forgotten once nothing of it can still run.
        if obsolete && (d.cancelled || !live && (run.is_some() || lost)) {
            seen.retire.insert(d.nonce.clone());
        }
    }
    let here: Vec<&Dispatch> = f
        .dispatches
        .iter()
        .filter(|d| d.head == f.revision.head && !seen.retire.contains(&d.nonce))
        .collect();
    let checks = if here.is_empty() {
        Vec::new()
    } else {
        github.check_runs(&repo, &f.revision.head)?
    };
    for name in config.checks.keys() {
        let owned: Vec<&&Dispatch> = here.iter().filter(|d| &d.check == name).collect();
        let proven = checks.iter().any(|c| {
            &c.name == name
                && c.status == "completed"
                && c.conclusion.as_deref() == Some("success")
                && c.external_id
                    .as_deref()
                    .is_some_and(|id| owned.iter().any(|d| d.nonce == id))
        });
        let state = if proven {
            Deferral::Done
        } else {
            let wakes: Vec<u64> = owned
                .iter()
                .filter(|d| !d.cancelled)
                .filter_map(|d| lapses(d, run_of(d), config))
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
        .filter(|(_, d)| **d != Deferral::Done)
        .collect();
    if missing.is_empty() {
        return None;
    }
    let wake = missing
        .iter()
        .filter_map(|(_, d)| match d {
            Deferral::InFlight { wake } => Some(*wake),
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

/// Landing permission needs every configured check's evidence, read afresh.
pub(crate) fn verify<G: Read + ?Sized>(github: &G, f: &Facts, now: u64) -> Result<()> {
    let seen = observe(github, f, now)?;
    for name in f.settings.deferred.checks.keys() {
        if seen.deferrals.get(name) != Some(&Deferral::Done) {
            return Err(format!("deferred check {name:?} has no successful evidence").into());
        }
    }
    Ok(())
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
        if record
            .dispatches
            .iter()
            .filter(mine)
            .any(|d| !d.cancelled && d.sent_at.saturating_add(config.start_timeout) > pass.now)
            || sent >= config.attempts as usize
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
            head: f.revision.head.clone(),
            nonce: nonce.clone(),
            sent_at: pass.now,
            run: None,
            cancelled: false,
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

/// Stop one run and remember that it was asked to.
pub(super) fn cancel<G: Write>(
    github: &G,
    f: &Facts,
    nonce: &str,
    record: &mut Record,
    data: &Data,
) -> Result<()> {
    let repo = f.repo.to_string();
    let d = record
        .dispatches
        .iter()
        .find(|d| d.nonce == nonce)
        .ok_or("dispatch missing")?;
    let run = d.run.or_else(|| {
        let workflow = &f.settings.deferred.checks.get(&d.check)?.workflow;
        github
            .workflow_runs(&repo, workflow)
            .ok()?
            .iter()
            .find(|r| titled(r, nonce))
            .map(|r| r.id)
    });
    let run = run.ok_or("run not found")?;
    let mark = |record: &mut Record, cancelled: bool| {
        let d = record
            .dispatches
            .iter_mut()
            .find(|d| d.nonce == nonce)
            .expect("found above");
        d.run = Some(run);
        d.cancelled = cancelled;
    };
    // A failed cancel is asked again by the next pass, which still sees the
    // run live; cancelling twice stops nothing twice.
    mark(record, false);
    data.save(f.number, record)?;
    github.cancel_run(&repo, run)?;
    mark(record, true);
    data.save(f.number, record)?;
    Ok(())
}
