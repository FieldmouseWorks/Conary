use std::collections::{BTreeMap, BTreeSet};

use super::{FixClaim, ProofData, ProofPlan, SealKey, TrainState, Unposted};
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
    pub record_id: Option<u64>,
    pub findings_answered: bool,
    pub unavailable_until: Option<u64>,
    pub queued: bool,
    /// An answer from the service which has not yet been posted to GitHub.
    pub answer: Option<Review>,
    /// A look already under way, even if its service is now unavailable.
    pub under_way: Option<Revision>,
    pub failed_at: Option<u64>,
    pub queue_failed_at: Option<u64>,
    pub missing: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Findings,
    Red,
    JudgeFailure,
    /// Repair the PR description without changing its head.
    Description,
    Conflict,
    Judge,
    Proof,
}

impl Kind {
    pub fn fixes_branch(self) -> bool {
        matches!(
            self,
            Self::Findings | Self::Red | Self::JudgeFailure | Self::Description | Self::Conflict
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

/// A proof's identity includes what runs, not just its status name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Recipe {
    Build(String),
    Run(Vec<String>),
    Job(String),
}

/// Machine settings, never selected by the PR's author.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub train: bool,
    pub job_login: Option<String>,
    pub proof_slots: Option<u32>,
    pub proof_timeout: u64,
    pub job_timeout: u64,
    pub judge_engines: BTreeSet<String>,
    pub reviewers: BTreeSet<String>,
    pub review_authors: BTreeSet<String>,
    pub verdict_authors: BTreeSet<String>,
    pub judge_key: Option<SealKey>,
    pub major_key: Option<SealKey>,
    pub bot_emails: BTreeSet<String>,
    pub renovate_email: Option<String>,
    pub renovate_login: Option<String>,
    pub batch_label: String,
    pub hold_label: String,
    pub first_label: String,
    pub owner_label: String,
    pub require_ci: bool,
    pub proofs: BTreeMap<String, Recipe>,
    pub settle_delay: u64,
    pub handoff_after: u64,
    pub session_handoff_after: u64,
    pub rounds: u32,
    pub stall_after: u64,
    pub hold_stall_after: u64,
    pub train_stall_after: u64,
    pub renovate_merge_after: u64,
    pub renovate_run: u64,
    pub renovate_sweep: bool,
}

impl Settings {
    pub fn machine(&self) -> BTreeSet<String> {
        self.proofs.keys().map(|name| name.to_lowercase()).collect()
    }
    pub fn proof_recipe(&self, name: &str) -> Option<(&String, &Recipe)> {
        self.proofs
            .iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case(name))
            .min_by_key(|(_, recipe)| match recipe {
                Recipe::Build(_) => 0,
                Recipe::Run(_) => 1,
                Recipe::Job(_) => 2,
            })
    }
    pub fn recipe(&self, name: &str) -> Option<&Recipe> {
        self.proof_recipe(name).map(|(_, recipe)| recipe)
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            train: false,
            job_login: None,
            proof_slots: None,
            proof_timeout: 10800,
            job_timeout: 3600,
            judge_engines: BTreeSet::from(["codex".into()]),
            reviewers: BTreeSet::from(["reviewer".into()]),
            review_authors: BTreeSet::from(["fieldmouseworks-steward[bot]".into()]),
            verdict_authors: BTreeSet::new(),
            judge_key: None,
            major_key: None,
            bot_emails: BTreeSet::from(
                ["49699333+dependabot[bot]@users.noreply.github.com".into()],
            ),
            renovate_email: None,
            renovate_login: None,
            batch_label: "upstream batch".into(),
            hold_label: "hold".into(),
            first_label: "first".into(),
            owner_label: "needs-peter".into(),
            require_ci: true,
            proofs: BTreeMap::new(),
            settle_delay: 300,
            handoff_after: 1800,
            session_handoff_after: 7200,
            rounds: 5,
            stall_after: 7200,
            hold_stall_after: 28800,
            train_stall_after: 7200,
            renovate_merge_after: 7200,
            renovate_run: 7200,
            renovate_sweep: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FailedPost {
    pub role: String,
    /// A rate limit or transport outage does not spend a review attempt.
    pub unavailable: bool,
}

/// All the facts read once for a pass. Local service answers, jobs and proofs
/// are supplied by the caller after [`super::read`]. No executor consumes this yet.
#[derive(Debug, Clone)]
pub struct Facts {
    pub repo: conary_core::op::Repo,
    pub title: String,
    pub description: String,
    pub branch: String,
    pub number: u64,
    pub revision: Revision,
    pub default_branch: String,
    pub base_tip: Option<String>,
    pub open: bool,
    pub draft: bool,
    pub trusted: bool,
    pub complete: bool,
    pub labels: BTreeSet<String>,
    pub merging: Merge,
    pub checks: Vec<Check>,
    pub has_ci: bool,
    pub open_threads: u32,
    pub changes_requested: bool,
    pub reviewers: BTreeMap<String, Reviewer>,
    pub judgment: Option<Judged>,
    pub jobs: BTreeMap<Kind, Job>,
    pub claims: Vec<FixClaim>,
    pub stopped_jobs: BTreeSet<String>,
    pub pushes: BTreeMap<String, String>,
    pub threads: BTreeMap<String, bool>,
    pub proof: Proof,
    pub proof_data: ProofData,
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
    pub bot_automerge_enabled: bool,
    pub rebase_requested: bool,
    pub rebase_removed: Vec<u64>,
    pub timeline_cut_at: Option<u64>,
    pub stacked_on: Option<u64>,
    pub open_prs: BTreeSet<u64>,
    pub first_prs: BTreeSet<u64>,
    pub base_held: bool,
    pub queued_merge: bool,
    pub train: TrainState,
    /// Whether merging into the current base would change nothing. None is
    /// unknown, never permission to land.
    pub empty: Option<bool>,
    pub updated_at: u64,
    pub created_at: Option<u64>,
    pub last_reply_at: u64,
    pub session: bool,
    /// The failed effect from the preceding execution, if any. Decisions
    /// never optimistically record an effect as having succeeded.
    pub failed: Option<Failure>,
    pub failed_review: Option<FailedPost>,
    pub succeeded: BTreeSet<Failure>,
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
    pub raised: BTreeSet<String>,
    pub latest_reviews: BTreeMap<String, u64>,
    pub consumed_reviews: BTreeSet<(String, Revision, u64)>,
    pub review_posts: BTreeMap<(String, Revision, u64), u32>,
    pub review_failures: BTreeMap<(String, Revision), (u32, u64)>,
    pub missing_reviews: BTreeMap<(String, Revision), (u64, bool)>,
    pub seen: u64,
    pub no_checks_since: Option<u64>,
    pub update_requested: Option<(String, u64)>,
    pub strikes: BTreeMap<Failure, u32>,
    pub retry: Option<(Failure, u64)>,
    /// Reviewer -> base -> head whose findings still need covering.
    pub lost_on: BTreeMap<String, BTreeMap<String, String>>,
    pub handed: BTreeMap<Kind, Revision>,
    pub lost_jobs: BTreeSet<String>,
    pub unanswered: BTreeMap<(Kind, Revision), BTreeSet<String>>,
    pub broken_jobs: BTreeMap<String, u64>,
    pub owner_saw: Option<Revision>,
    pub answered_jobs: BTreeSet<String>,
    pub stopping: BTreeSet<String>,
    pub retired_jobs: BTreeSet<String>,
    pub unposted: BTreeMap<Kind, (String, Unposted)>,
    pub push_looks: BTreeMap<(String, String), (u32, u64)>,
    pub wait: Option<Wait>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    QueueReview(String),
    PostReview(String),
    PostFix(String),
    HandFix(Kind),
    StopJob(String),
    Prove,
    PostProof,
    Judge,
    UpdateBranch,
    Settle,
    Merge,
    JoinTrain,
    LeaveTrain,
    DisableAutoMerge,
    CloseAlreadyLanded,
    ListOwner,
    Retry { kind: Failure, at: u64 },
    Wait { until: Option<u64> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitOn {
    Steward,
    Hold,
    Sweep,
    Renovate,
    Train,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerNotice {
    OpenOverDay,
    NoReviewer,
    ReviewFailures,
    UnpostedReview,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Next {
    pub stage: Stage,
    pub step: Step,
    pub description: String,
    pub overdue_at: Option<u64>,
    pub proof: Option<ProofPlan>,
    pub wait_on: Option<WaitOn>,
    pub on_usage: bool,
    /// A digest decision, independent of the one executable intention.
    pub owner_notice: Option<OwnerNotice>,
}
