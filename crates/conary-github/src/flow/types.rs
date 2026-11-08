use std::collections::{BTreeMap, BTreeSet};

use conary_core::policy::Classified;
use serde::{Deserialize, Serialize};

/// Keys from burrow's stages.py, including its null stage.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Stage {
    Review,
    Reviewer,
    Fix,
    Ci,
    ProofSlot,
    Proof,
    ReleaseAge,
    Train,
    Merge,
    Owner,
    Base,
    #[default]
    #[serde(untagged)]
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Merge {
    Ready,
    Update,
    Conflict,
    Unknown,
    Refused,
}

impl Merge {
    /// Every mergeStateStatus, and a fail-closed answer for a new value.
    pub fn from_github(s: &str) -> Self {
        match s {
            "CLEAN" | "HAS_HOOKS" | "UNSTABLE" | "BLOCKED" => Self::Ready,
            "BEHIND" => Self::Update,
            "DIRTY" => Self::Conflict,
            "UNKNOWN" | "DRAFT" => Self::Unknown,
            _ => Self::Refused,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ci {
    None,
    Green,
    Pending,
    Red,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    pub state: Ci,
}

/// Scope of a verdict. Description and base are as much its input as head.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Revision {
    pub head: String,
    pub base: String,
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Clean,
    Findings,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    pub revision: Revision,
    pub verdict: Verdict,
    pub at: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reviewer {
    pub review: Option<Review>,
    pub rounds: u32,
    pub unavailable_until: Option<u64>,
    pub queued: bool,
    /// An answer from the service which has not yet been posted to GitHub.
    pub answer: Option<Review>,
    /// A look already under way, even if its service is now unavailable.
    pub under_way: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Findings,
    Red,
    JudgeFailure,
    Conflict,
    Judge,
    Proof,
}

impl Kind {
    pub fn fixes_branch(self) -> bool {
        matches!(
            self,
            Self::Findings | Self::Red | Self::JudgeFailure | Self::Conflict
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Queued,
    Running,
    Unknown,
    Done,
    Broken,
}

impl JobState {
    pub fn live(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::Unknown)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub id: String,
    pub revision: Revision,
    pub state: JobState,
    pub started: u64,
    pub repeated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Judgment {
    Land,
    Owner,
    Failing,
    Misdescribed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Judged {
    pub revision: Revision,
    pub verdict: Judgment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dependency {
    Minor,
    Major,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Major {
    Cleared,
    Held,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Proof {
    #[default]
    NotNeeded,
    Needed,
    InLine(u32),
    Running,
    Passed,
    Failed,
    /// Verdict is stored locally, but still needs posting to GitHub.
    Unposted,
}

/// Machine settings, never selected by the PR's author.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub train: bool,
    pub require_ci: bool,
    pub proofs: BTreeSet<String>,
    pub settle_delay: u64,
    pub handoff_after: u64,
    pub session_handoff_after: u64,
    pub rounds: u32,
    pub stall_after: u64,
    pub hold_stall_after: u64,
    pub train_stall_after: u64,
    pub renovate_merge_after: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            train: false,
            require_ci: true,
            proofs: BTreeSet::new(),
            settle_delay: 300,
            handoff_after: 1800,
            session_handoff_after: 7200,
            rounds: 5,
            stall_after: 7200,
            hold_stall_after: 28800,
            train_stall_after: 7200,
            renovate_merge_after: 7200,
        }
    }
}

/// All the facts read once for a pass. Local service answers, jobs and proofs
/// are supplied by the caller after [`super::read`]. No executor consumes this yet.
#[derive(Debug, Clone)]
pub struct Facts {
    pub revision: Revision,
    pub open: bool,
    pub draft: bool,
    pub trusted: bool,
    pub complete: bool,
    pub labels: BTreeSet<String>,
    pub merging: Merge,
    pub checks: Vec<Check>,
    pub open_threads: u32,
    pub changes_requested: bool,
    pub reviewers: BTreeMap<String, Reviewer>,
    pub judgment: Option<Judged>,
    pub jobs: BTreeMap<Kind, Job>,
    pub proof: Proof,
    pub settings: Settings,
    pub classified: Classified,
    pub unseen: bool,
    pub authorship_known: bool,
    pub needs_judge: bool,
    pub judge_asked: bool,
    pub dependency: Option<Dependency>,
    pub renovate: bool,
    pub bot_edited: Option<bool>,
    pub gate_edited: bool,
    pub major: Option<Major>,
    pub release_at: Option<u64>,
    pub bot_merges: bool,
    pub stacked_on: Option<u64>,
    pub base_held: bool,
    /// Whether merging into the current base would change nothing. None is
    /// unknown, never permission to land.
    pub empty: Option<bool>,
    pub updated_at: u64,
    pub last_reply_at: u64,
    pub session: bool,
    /// The failed effect from the preceding execution, if any. Decisions
    /// never optimistically record an effect as having succeeded.
    pub failed: Option<Failure>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Failure {
    Status,
    Update,
    Merge,
    Empty,
    Close,
    Mergeable,
    Job,
    Build,
    Eval,
    Post,
    Answer,
    Lost,
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wait {
    pub head: String,
    pub base: String,
    pub stage: Stage,
    pub description: String,
    pub failure: Option<Failure>,
    pub since: u64,
    pub due: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Memory {
    pub head: String,
    pub seen: u64,
    pub no_checks_since: Option<u64>,
    pub strikes: BTreeMap<Failure, u32>,
    pub retry: Option<(Failure, u64)>,
    /// Reviewer -> base -> head whose findings still need covering.
    pub lost_on: BTreeMap<String, BTreeMap<String, String>>,
    pub handed: BTreeMap<Kind, Revision>,
    pub lost_jobs: BTreeSet<String>,
    pub broken_jobs: BTreeMap<String, u64>,
    pub owner_saw: Option<Revision>,
    pub wait: Option<Wait>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    QueueReview(String),
    PostReview(String),
    HandFix(Kind),
    Prove,
    PostProof,
    Judge,
    UpdateBranch,
    Settle,
    Merge,
    JoinTrain,
    CloseAlreadyLanded,
    ListOwner,
    Retry { kind: Failure, at: u64 },
    Wait { until: Option<u64> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Next {
    pub stage: Stage,
    pub step: Step,
    pub description: String,
    pub overdue_at: Option<u64>,
}
