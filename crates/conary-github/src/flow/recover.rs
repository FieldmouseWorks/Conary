//! `recover-by-kind`: at an expired deadline of a wait, the flow brings the
//! stage back by the first way GitHub allows that applies: a head behind its
//! base that merges cleanly is updated with it, then a cancelled check run
//! nothing replaced, or one that timed out and no fix job has, is run again
//! as its kind allows (a GitHub Actions workflow run's rerun, another App's
//! check suite's re-request). A commit status, or a run in no suite the flow
//! read, has no such way, and is never asked for.
//!
//! The flow asks once in a stall, and each way once on a head: whatever
//! GitHub answers, repeated events, wakes and restarts never ask again
//! ([`super::execute`] saves the ask before it's made). A refusal is quoted
//! as GitHub gave it. After that the stall stays overdue.

use serde::{Deserialize, Serialize};

use super::*;

/// Whether a repository's attested workflow runs require single-attempt
/// attestation cohesion (every job in the workflow run from the same attempt),
/// structurally forbidding partial `rerun-failed-jobs` recovery.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RerunPolicy {
    /// Single-attempt attestation cohesion is required: only full workflow
    /// reruns (`POST /repos/{owner}/{repo}/actions/runs/{run_id}/rerun`) are
    /// permitted.
    #[default]
    FullWorkflowOnly,
    /// No single-attempt attestation cohesion is required: partial
    /// `rerun-failed-jobs` recovery is allowed.
    FailedJobsAllowed,
}

impl RerunPolicy {
    /// Derive the rerun policy structurally from the repository's evidence
    /// configuration: any `conaryd`-attested repository requires single-attempt
    /// attestation cohesion and can never select partial `rerun-failed-jobs`.
    pub fn for_evidence(evidence: Option<&Evidence>) -> Self {
        match evidence {
            Some(_) => Self::FullWorkflowOnly,
            None => Self::FailedJobsAllowed,
        }
    }

    /// Select the workflow rerun recovery primitive permitted by this policy
    /// for workflow run `run`.
    pub fn workflow_recovery(self, run: u64) -> Recovery {
        match self {
            Self::FullWorkflowOnly => Recovery::Rerun(run),
            Self::FailedJobsAllowed => Recovery::RerunFailedJobs(run),
        }
    }

    /// Rewrite a recovery candidate according to this policy.
    pub fn apply(self, recovery: Recovery) -> Recovery {
        match recovery {
            Recovery::Rerun(run) | Recovery::RerunFailedJobs(run) => self.workflow_recovery(run),
            other => other,
        }
    }

    /// Whether `recovery` is permitted under this policy.
    pub fn permits(self, recovery: &Recovery) -> bool {
        !matches!(
            (self, recovery),
            (Self::FullWorkflowOnly, Recovery::RerunFailedJobs(_))
        )
    }
}

/// One way GitHub brings a stalled stage back.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Recovery {
    /// Update the head with its base, which it's behind and merges cleanly.
    Update,
    /// Run a GitHub Actions workflow run again in full, by the run's id
    /// (`POST /repos/{owner}/{repo}/actions/runs/{run_id}/rerun`).
    Rerun(u64),
    /// Run only the failed jobs of a GitHub Actions workflow run again, by the
    /// run's id (`POST /repos/{owner}/{repo}/actions/runs/{run_id}/rerun-failed-jobs`).
    /// Structurally forbidden on single-attempt-attested repositories.
    RerunFailedJobs(u64),
    /// Ask the App whose check suite it is to run it again, by the suite's id.
    Rerequest(u64),
}

impl Recovery {
    /// Whether `self` and `other` target the same underlying GitHub resource
    /// (treating full and failed-jobs reruns of the same workflow run id as
    /// one target so a workflow run is never asked twice on one head).
    pub(super) fn same_target(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Rerun(a) | Self::RerunFailedJobs(a),
                Self::Rerun(b) | Self::RerunFailedJobs(b),
            ) => a == b,
            _ => self == other,
        }
    }
}

/// What became of a recovery the flow asked GitHub for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Answer {
    /// Asked, its answer not saved: a restart in between doesn't ask again.
    Asked,
    Accepted,
    /// GitHub refused it, in its own words.
    Refused(String),
    /// GitHub couldn't be asked, or its answer was lost: the error that
    /// came back instead.
    Unanswered(String),
}

/// A recovery the flow asked for, and GitHub's answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recovered {
    pub head: String,
    /// When the stall it was for began: its wait's `since`.
    pub since: u64,
    pub recovery: Recovery,
    /// The checks it brings back; none for an update.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<String>,
    pub answer: Answer,
    /// True when triggered immediately for an uncertain CI outcome rather than
    /// after an overdue stall deadline.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub immediate: bool,
}

impl Recovered {
    /// What the status says of it, GitHub's own words first when it
    /// refused, since GitHub keeps only 140 characters of a status.
    pub fn describe(&self) -> String {
        let what = match self.recovery {
            Recovery::Update => "update the head with its base".to_owned(),
            Recovery::Rerun(_) => format!("rerun {}", self.checks.join(", ")),
            Recovery::RerunFailedJobs(_) => {
                format!("rerun failed jobs for {}", self.checks.join(", "))
            }
            Recovery::Rerequest(_) => format!("rerequest {}", self.checks.join(", ")),
        };
        let prefix = if self.immediate {
            "Uncertain CI"
        } else {
            "Overdue"
        };
        let text = match &self.answer {
            Answer::Asked => format!("{prefix}; asking GitHub to {what}"),
            Answer::Accepted => format!("{prefix}; GitHub was asked to {what}"),
            Answer::Refused(said) if self.immediate => {
                format!("{said} (GitHub, refusing to {what})")
            }
            Answer::Refused(said) => format!("{said} (GitHub, refusing to {what}; overdue)"),
            Answer::Unanswered(why) => {
                format!("{prefix}; GitHub didn't answer the ask to {what}: {why}")
            }
        };
        text.chars().take(140).collect()
    }
}

/// What an expired wait shows, or does: a recovery to ask for, or the one
/// its stall already asked for.
pub(super) struct Look {
    pub step: Option<Step>,
    pub recovery: Recovered,
}

/// At an expired deadline of a wait (`wait`, its stall), the first recovery
/// that applies and wasn't asked for on this head, if this stall had none
/// yet; else the one this stall had, to show while it waits. A red check's
/// fix that can't be handed stalls the same way, and [`unhanded`] shows the
/// recovery its stall had while it still can't.
pub(super) fn look(
    f: &Facts,
    m: &Memory,
    stage: Stage,
    step: &Step,
    wait: &Wait,
    now: u64,
) -> Option<Look> {
    let waits = matches!(step, Step::Wait { .. });
    if !(waits || stage == Stage::Fix && *step == Step::HandFix(Kind::Red)) || now < wait.due {
        return None;
    }
    let mine = |r: &&Recovered| r.head == f.revision.head;
    if let Some(r) = m
        .recovered
        .iter()
        .rev()
        .filter(mine)
        .find(|r| r.since == wait.since)
    {
        return Some(Look {
            step: None,
            recovery: r.clone(),
        });
    }
    if let Some((recovery, checks)) = first(f, m, stage) {
        return Some(Look {
            step: Some(Step::Recover(recovery.clone())),
            recovery: Recovered {
                head: f.revision.head.clone(),
                since: wait.since,
                recovery,
                checks,
                answer: Answer::Asked,
                immediate: false,
            },
        });
    }
    None
}

/// The first recovery for a stall at `stage` that applies and wasn't asked
/// for on this head. A head is updated only at a wait on CI or the merge,
/// never one its bot or a fix job keeps, or one stacked on another PR. A
/// cancelled run is run again at a wait on CI; a timed-out one, red, at a
/// stalled fix no fix job or dependency sweep has.
fn first(f: &Facts, m: &Memory, stage: Stage) -> Option<(Recovery, Vec<String>)> {
    let spent = |r: &Recovery| {
        m.recovered
            .iter()
            .any(|d| d.head == f.revision.head && d.recovery.same_target(r))
    };
    if matches!(stage, Stage::Ci | Stage::Merge)
        && f.merging == Merge::Update
        && f.dependency.is_none()
        && f.stacked_on.is_none()
        && !spent(&Recovery::Update)
    {
        return Some((Recovery::Update, Vec::new()));
    }
    let fixing = f.dependency.is_some()
        || f.jobs
            .iter()
            .any(|(kind, job)| kind.fixes_branch() && job.state.live());
    let stopped = match stage {
        Stage::Ci => &f.stopped,
        Stage::Fix if !fixing => &f.timed_out,
        _ => return None,
    };
    stopped
        .iter()
        .find(|(r, _)| !spent(r))
        .map(|(r, checks)| (f.rerun_policy.apply(r.clone()), checks.clone()))
}

/// After the effect, what the decision shows of the recovery it asked for:
/// GitHub's answer, as [`super::execute`] saved it. A recovery the pass
/// put off for something else wasn't asked for, and isn't shown.
pub(super) fn answered(next: &mut Next, m: &Memory) {
    let Step::Recover(_) = next.step else {
        if next
            .recovery
            .as_ref()
            .is_some_and(|r| r.answer == Answer::Asked)
        {
            next.recovery = None;
        }
        return;
    };
    let Some(asked) = &next.recovery else {
        return;
    };
    if let Some(r) =
        m.recovered.iter().rev().find(|r| {
            r.head == asked.head && r.since == asked.since && r.recovery == asked.recovery
        })
    {
        next.description = r.describe();
        next.recovery = Some(r.clone());
    }
}

/// A decided step the pass can't take (`why`) waits instead. A fix that
/// can't be handed is the stall a recovery was asked for: the status keeps
/// showing that ask and GitHub's answer, not `why`, for as long as it lasts.
pub(super) fn unhanded(next: &mut Next, why: String) {
    next.step = Step::Wait { until: None };
    next.description = match &next.recovery {
        Some(r) => r.describe(),
        None => why,
    };
}

/// A fix the pass hands goes to its job: the stall's recovery is no longer
/// what the PR waits on, and isn't shown.
pub(super) fn handed(next: &mut Next) {
    if let Step::HandFix(_) = next.step {
        next.recovery = None;
    }
}
