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
#[derive(
    Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct Revision {
    pub head: String,
    pub base: String,
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    Clean,
    Findings,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Review {
    pub revision: Revision,
    pub verdict: Verdict,
    /// Producer completion time, preserved across posting retries. Zero is unknown.
    pub at: u64,
}

/// Findings whose posting/coverage was lost. Legacy state has no ordering
/// evidence until this library observes it; a REST record ID supplies none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LostReview {
    Legacy { head: String, after: Option<u64> },
    Findings { revision: Revision, at: u64 },
}
impl From<String> for LostReview {
    fn from(head: String) -> Self {
        Self::Legacy { head, after: None }
    }
}
impl LostReview {
    pub fn head(&self) -> &str {
        match self {
            Self::Legacy { head, .. } => head,
            Self::Findings { revision, .. } => &revision.head,
        }
    }
    pub(super) fn after(&self) -> Option<u64> {
        match self {
            Self::Legacy { after, .. } => *after,
            Self::Findings { at, .. } => Some(*at),
        }
    }
    pub(super) fn observe(&mut self, now: u64) {
        if let Self::Legacy { after, .. } = self {
            after.get_or_insert(now);
        }
    }
    pub(super) fn findings(review: &Review, now: u64) -> Self {
        if review.at == 0 {
            Self::Legacy {
                head: review.revision.head.clone(),
                after: Some(now),
            }
        } else {
            Self::Findings {
                revision: review.revision.clone(),
                at: review.at,
            }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reviewer {
    pub review: Option<Review>,
    pub rounds: u32,
    pub record_id: Option<u64>,
    /// Original reviewed head, retained when equivalent-tree coverage carries.
    pub reviewed_head: Option<String>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub revision: Revision,
    pub state: JobState,
    pub started: u64,
    pub repeated: bool,
}

pub use conary_core::plugin::{Judgment, Trigger};

/// Per revision: the highest authenticated judge generation, and its verdict
/// with any provenance (`None` when that generation's words conflict).
pub type JudgeIssuance = BTreeMap<Revision, (u64, Option<JudgeResult>)>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Judged {
    pub revision: Revision,
    pub verdict: Judgment,
    pub provenance: Option<conary_core::plugin::Provenance>,
}

/// Old saved issuances are plain verdicts. New ones retain their provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JudgeResult {
    Recorded {
        verdict: Judgment,
        provenance: conary_core::plugin::Provenance,
    },
    Legacy(Judgment),
}
impl Judged {
    pub(super) fn result(&self) -> JudgeResult {
        match &self.provenance {
            Some(provenance) => JudgeResult::Recorded {
                verdict: self.verdict,
                provenance: provenance.clone(),
            },
            None => JudgeResult::Legacy(self.verdict),
        }
    }
    pub(super) fn from_result(revision: Revision, result: JudgeResult) -> Self {
        match result {
            JudgeResult::Recorded {
                verdict,
                provenance,
            } => Self {
                revision,
                verdict,
                provenance: Some(provenance),
            },
            JudgeResult::Legacy(verdict) => Self {
                revision,
                verdict,
                provenance: None,
            },
        }
    }
    /// A judge plugin's answer for `revision`; other answers are not judgments.
    pub fn from_answer(revision: Revision, answer: &conary_core::plugin::Answer) -> Option<Self> {
        match answer {
            conary_core::plugin::Answer::Judge { verdict, .. } => Some(Self {
                revision,
                verdict: *verdict,
                provenance: None,
            }),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dependency {
    Minor,
    Major,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Recipe {
    Build(String),
    Run(Vec<String>),
    Job(String),
}

/// Machine settings, never selected by the PR's author.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
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
    pub people: BTreeSet<String>,
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
            people: BTreeSet::new(),
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailedPost {
    pub role: String,
    /// A rate limit or transport outage does not spend a review attempt.
    pub unavailable: bool,
}

/// All the facts read once for a pass. Local service answers, jobs and proofs
/// are supplied by the caller after [`super::read`]. The pass executor supplies these observations.
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
    pub owner_approved: bool,
    pub reviewers: BTreeMap<String, Reviewer>,
    pub judgment: Option<Judged>,
    pub judgment_generation: Option<u64>,
    pub major_generation: Option<u64>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Wait {
    pub head: String,
    pub base: String,
    pub stage: Stage,
    pub description: String,
    pub failure: Option<Failure>,
    pub since: u64,
    pub due: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Memory {
    pub head: String,
    pub raised: BTreeSet<String>,
    #[serde(with = "super::persist::pairs")]
    pub latest_reviews: BTreeMap<String, u64>,
    /// Authenticated issuance high-water marks, including same-generation conflicts.
    #[serde(with = "super::persist::judge_issuance")]
    pub judge_issuance: JudgeIssuance,
    #[serde(with = "super::persist::pairs")]
    pub major_issuance: BTreeMap<Revision, (u64, Option<Major>)>,
    pub consumed_reviews: BTreeSet<(String, Revision, u64)>,
    #[serde(with = "super::persist::pairs")]
    pub review_posts: BTreeMap<(String, Revision, u64), u32>,
    #[serde(with = "super::persist::pairs")]
    pub review_failures: BTreeMap<(String, Revision), (u32, u64)>,
    #[serde(with = "super::persist::pairs")]
    pub missing_reviews: BTreeMap<(String, Revision), (u64, bool)>,
    pub seen: u64,
    pub no_checks_since: Option<u64>,
    pub update_requested: Option<(String, u64)>,
    #[serde(with = "super::persist::pairs")]
    pub strikes: BTreeMap<Failure, u32>,
    pub retry: Option<(Failure, u64)>,
    /// Reviewer -> base -> the original findings and their ordering evidence.
    #[serde(with = "super::persist::pairs")]
    pub lost_on: BTreeMap<String, BTreeMap<String, LostReview>>,
    #[serde(with = "super::persist::pairs")]
    pub handed: BTreeMap<Kind, Revision>,
    pub lost_jobs: BTreeSet<String>,
    #[serde(with = "super::persist::pairs")]
    pub unanswered: BTreeMap<(Kind, Revision), BTreeSet<String>>,
    #[serde(with = "super::persist::pairs")]
    pub broken_jobs: BTreeMap<String, u64>,
    pub answered_jobs: BTreeSet<String>,
    pub stopping: BTreeSet<String>,
    pub retired_jobs: BTreeSet<String>,
    #[serde(with = "super::persist::pairs")]
    pub unposted: BTreeMap<Kind, (String, Unposted)>,
    #[serde(with = "super::persist::pairs")]
    pub push_looks: BTreeMap<(String, String), (u32, u64)>,
    pub wait: Option<Wait>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Step {
    QueueReview(String),
    PostReview(String),
    PostFix(String),
    HandFix(Kind),
    StopJob(String),
    Prove,
    PostProof,
    /// Ask a judge, for these reasons. Empty when a judge already started on
    /// this head and base is all that holds (the request then carries that
    /// judge's reasons), and in decisions saved before steps carried them.
    Judge(Vec<Trigger>),
    UpdateBranch,
    Settle,
    Merge,
    JoinTrain,
    LeaveTrain,
    DisableAutoMerge,
    CloseAlreadyLanded,
    ListOwner,
    Retry {
        kind: Failure,
        at: u64,
    },
    Wait {
        until: Option<u64>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WaitOn {
    Steward,
    Hold,
    Sweep,
    Renovate,
    Train,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OwnerNotice {
    OpenOverDay,
    NoReviewer,
    ReviewFailures,
    UnpostedReview,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Next {
    pub stage: Stage,
    #[serde(deserialize_with = "super::persist::step::deserialize")]
    pub step: Step,
    pub description: String,
    /// Earliest explicit wait or owned worker deadline, in Unix seconds.
    #[serde(default)]
    pub wait_until: Option<u64>,
    pub overdue_at: Option<u64>,
    pub proof: Option<ProofPlan>,
    pub wait_on: Option<WaitOn>,
    pub on_usage: bool,
    /// A digest decision, independent of the one executable intention.
    pub owner_notice: Option<OwnerNotice>,
}
