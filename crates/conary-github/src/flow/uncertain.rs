//! Typed uncertain CI outcomes (`negative` and attested gate `error`) and
//! policy-driven, single-attempt-safe recovery (FieldmouseWorks/Conary#602).
//!
//! When a configured `negative` check fails without a valid attested `pass`
//! verdict (either attesting `error`, or producing no negative attestation at
//! all because the base check flaked or infrastructure broke), or when a
//! required gate check carries a validly signed `error` attestation, the check
//! did not judge the PR. On its first attempt (`run_attempt == 1`), the flow
//! recovers once via the repository's [`RerunPolicy`] (full workflow rerun on
//! single-attempt-attested repositories); on `run_attempt >= 2` (or if
//! recovery cannot be made), it stops retrying and escalates as a base/machine
//! failure rather than dispatching a PR fix job.

use std::collections::BTreeSet;

use conary_core::op::Outcome;

use super::decide::{Decision, at, wait};
use super::*;
use crate::api::CheckRun;
use crate::attest::{Subject, attested, gate_error};

/// A check on the current head that ended without judging the PR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UncertainCheck {
    pub name: String,
    pub why: String,
    /// Highest observed workflow run attempt (from the job and workflow run).
    pub attempt: Option<u64>,
    /// Policy-selected recovery primitive when eligible (`attempt == 1` and
    /// workflow run completed).
    pub recovery: Option<Recovery>,
    /// True while the workflow run is still in progress (other jobs still
    /// running, or a newer attempt already running while check runs lag).
    pub running: bool,
}

/// What inspecting a completed failed check run's attestation revealed.
enum Verdict {
    /// A PR-code verdict (attested `negative` `pass`, or a required gate that
    /// attested `fail` or had no `error` attestation).
    PrFailed,
    /// No PR verdict: the check could not judge the PR.
    Uncertain(String),
    /// GitHub could not be read; degrade safely and do not guess.
    Degraded(String),
}

/// Classify completed failed `negative` and `require` checks using the
/// repository's attestation contract, and record the repository's
/// [`RerunPolicy`].
pub(crate) fn hydrate<G: Write>(github: &G, facts: &mut Facts, evidence: Option<&Evidence>) {
    facts.rerun_policy = RerunPolicy::for_evidence(evidence);
    let Some(evidence) = evidence else {
        return;
    };
    let Ok(trust) = evidence.trust() else {
        return;
    };
    let repo = facts.repo.to_string();
    let head = facts.revision.head.clone();
    let negative_name = evidence.negative.trim();
    let mut pr_failed = BTreeSet::new();
    let mut degraded_names = BTreeSet::new();
    let mut uncertain_runs: Vec<(&CheckRun, String)> = Vec::new();
    let mut cached_base: Option<std::result::Result<String, String>> = None;

    for run in &facts.effective_runs {
        if run.status != "completed"
            || matches!(
                run.conclusion.as_deref(),
                Some("success" | "neutral" | "skipped" | "cancelled")
            )
        {
            continue;
        }
        let is_negative = !negative_name.is_empty() && run.name == negative_name;
        let is_required = evidence.require.iter().any(|r| r == &run.name);
        if !is_negative && !is_required {
            continue;
        }
        let verdict = if is_negative {
            classify_negative(github, facts, &repo, &head, run, &trust, &mut cached_base)
        } else {
            classify_required(github, facts, &head, run, &trust)
        };
        match verdict {
            Verdict::PrFailed => {
                pr_failed.insert(run.name.clone());
            }
            Verdict::Degraded(why) => {
                facts.reads.lookup(run.name.clone(), why);
                degraded_names.insert(run.name.clone());
            }
            Verdict::Uncertain(why) => {
                uncertain_runs.push((run, why));
            }
        }
    }

    let mut uncertain = Vec::new();
    let mut uncertain_names = BTreeSet::new();
    for (run, why) in uncertain_runs {
        if pr_failed.contains(&run.name) || degraded_names.contains(&run.name) {
            continue;
        }
        match inspect_attempt(github, &repo, run, &facts.suites, facts.rerun_policy) {
            Ok((attempt, recovery, running)) => {
                uncertain_names.insert(run.name.clone());
                uncertain.push(UncertainCheck {
                    name: run.name.clone(),
                    why,
                    attempt,
                    recovery,
                    running,
                });
            }
            Err(why) => {
                facts.reads.lookup(run.name.clone(), why);
                degraded_names.insert(run.name.clone());
            }
        }
    }

    for check in &mut facts.checks {
        if check.state == Ci::Red {
            if degraded_names.contains(&check.name) {
                check.state = Ci::Pending;
            } else if uncertain_names.contains(&check.name) {
                check.state = Ci::Uncertain;
            }
        }
    }
    facts.uncertain = uncertain;
}

fn classify_negative<G: Write>(
    github: &G,
    facts: &Facts,
    repo: &str,
    head: &str,
    run: &CheckRun,
    trust: &crate::attest::Trust,
    cached_base: &mut Option<std::result::Result<String, String>>,
) -> Verdict {
    let base = cached_base.get_or_insert_with(|| {
        let Some(tip) = facts.base_tip.as_deref() else {
            return Err("negative base unknown".into());
        };
        github
            .merge_base(repo, tip, head)
            .map_err(|e| super::degraded::why(&e))
    });
    let base = match base {
        Ok(base) => base.clone(),
        Err(why) => return Verdict::Degraded(why.clone()),
    };
    match attested(
        github,
        github,
        &facts.repo,
        Subject::Negative { base: &base, head },
        run,
        trust,
    ) {
        Err(e) => Verdict::Degraded(super::degraded::why(&e)),
        Ok(Ok(att)) if att.result == Outcome::Pass => Verdict::PrFailed,
        Ok(Ok(att)) if att.result == Outcome::Error => Verdict::Uncertain(
            att.error
                .unwrap_or_else(|| "negative run could not judge the change".into()),
        ),
        Ok(Ok(_)) => Verdict::PrFailed,
        Ok(Err(why)) => Verdict::Uncertain(why),
    }
}

fn classify_required<G: Write>(
    github: &G,
    facts: &Facts,
    head: &str,
    run: &CheckRun,
    trust: &crate::attest::Trust,
) -> Verdict {
    match gate_error(github, github, &facts.repo, head, run, trust) {
        Err(e) => Verdict::Degraded(super::degraded::why(&e)),
        Ok(Some(why)) => Verdict::Uncertain(why),
        Ok(None) => Verdict::PrFailed,
    }
}

fn inspect_attempt<G: Write>(
    github: &G,
    repo: &str,
    run: &CheckRun,
    suites: &std::collections::BTreeMap<u64, Suite>,
    policy: RerunPolicy,
) -> Result<(Option<u64>, Option<Recovery>, bool), String> {
    let suite = run.check_suite.as_ref().and_then(|s| suites.get(&s.id));
    let suite_running = suite.is_some_and(|s| s.status != "completed");
    let job = github
        .job(repo, run.id)
        .map_err(|e| super::degraded::why(&e))?;
    if let Some(job) = job {
        let wf_run_id = suite
            .and_then(|s| s.workflow.as_ref())
            .and_then(|w| w.run)
            .unwrap_or(job.run_id);
        let wf = github
            .workflow_run(repo, wf_run_id)
            .map_err(|e| super::degraded::why(&e))?;
        let attempt = job.run_attempt.max(wf.run_attempt);
        let running = suite_running || wf.status != "completed";
        let recovery = (!running && attempt == 1).then(|| policy.workflow_recovery(wf_run_id));
        Ok((Some(attempt), recovery, running))
    } else {
        let recovery = (!suite_running)
            .then(|| {
                suite
                    .filter(|s| !s.app.is_empty() && s.app != "github-actions")
                    .and_then(|_| run.check_suite.as_ref().map(|s| Recovery::Rerequest(s.id)))
            })
            .flatten();
        Ok((None, recovery, suite_running))
    }
}

/// Whether `rec` has already been asked for on this head (including another
/// rerun variant of the same workflow run id).
fn prior<'a>(m: &'a Memory, head: &str, rec: &Recovery) -> Option<&'a Recovered> {
    m.recovered
        .iter()
        .rev()
        .find(|r| r.head == head && r.recovery.same_target(rec))
}

/// Whether the uncertain checks are waiting for an in-progress workflow run or
/// an already-accepted rerun to show up (rather than escalated).
fn waiting_for_rerun(f: &Facts, m: &Memory) -> bool {
    if f.uncertain.iter().any(|u| u.running) {
        return true;
    }
    f.uncertain.iter().any(|u| {
        u.attempt.is_none_or(|a| a == 1)
            && u.recovery.as_ref().is_some_and(|rec| {
                prior(m, &f.revision.head, rec).is_some_and(|r| r.answer == Answer::Accepted)
            })
    })
}

/// Decide the next step when one or more checks ended uncertainly without a PR
/// verdict: wait for an in-progress workflow run, trigger one policy-driven
/// rerun on attempt 1, or stop retrying and escalate as a base/machine failure.
pub(crate) fn decide(f: &Facts, m: &Memory) -> Option<Decision> {
    if f.uncertain.is_empty() {
        return None;
    }
    if f.uncertain.iter().any(|u| u.running) {
        let names: Vec<_> = f
            .uncertain
            .iter()
            .filter(|u| u.running)
            .map(|u| u.name.as_str())
            .collect();
        return Some(wait(
            Stage::Ci,
            None,
            &format!(
                "Settled; waiting for workflow run to finish before recovering uncertain CI: {}",
                names.join(", ")
            ),
        ));
    }
    if let Some((u, rec)) = f.uncertain.iter().find_map(|u| {
        let rec = u.recovery.as_ref()?;
        prior(m, &f.revision.head, rec)
            .is_none()
            .then_some((u, rec))
    }) {
        return Some(at(
            Stage::Ci,
            Step::Recover(rec.clone()),
            &format!("Uncertain CI; asking GitHub to rerun {}", u.name),
        ));
    }
    if waiting_for_rerun(f, m) {
        let names: Vec<_> = f.uncertain.iter().map(|u| u.name.as_str()).collect();
        return Some(wait(
            Stage::Ci,
            None,
            &format!(
                "Uncertain CI; waiting for rerun of {} to start",
                names.join(", ")
            ),
        ));
    }
    if let Some(r) = f.uncertain.iter().find_map(|u| {
        u.recovery
            .as_ref()
            .and_then(|rec| prior(m, &f.revision.head, rec))
            .filter(|r| matches!(r.answer, Answer::Refused(_) | Answer::Unanswered(_)))
    }) {
        return Some(wait(Stage::Ci, None, &r.describe()));
    }
    let first = &f.uncertain[0];
    let description = match first.attempt {
        Some(attempt) => format!(
            "Uncertain CI on attempt {attempt} (base/machine failure): {} ({})",
            first.name, first.why
        ),
        None => format!(
            "Uncertain CI (base/machine failure): {} ({})",
            first.name, first.why
        ),
    };
    Some(wait(Stage::Ci, None, &description))
}

/// Whether this decision is an escalated uncertain CI wait (stopped retrying
/// after `run_attempt >= 2`, or recovery refused/unavailable), which is overdue
/// immediately for base/machine escalation.
pub(crate) fn escalated(f: &Facts, m: &Memory, stage: Stage, step: &Step) -> bool {
    !f.uncertain.is_empty()
        && stage == Stage::Ci
        && matches!(step, Step::Wait { .. })
        && !waiting_for_rerun(f, m)
}
