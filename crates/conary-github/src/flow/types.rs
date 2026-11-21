use std::collections::{BTreeMap, BTreeSet};

use super::{
    FixClaim, Level, ProofData, ProofPlan, Rank, SealKey, StallAfter, TrainState, Unposted,
};
use conary_core::policy::Classified;
use serde::{Deserialize, Serialize};

/// Keys from burrow's stages.py, including its null stage.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Stage {
    Review,
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

/// What a reviewer last said of a PR's head.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    /// A review of the head is under way.
    Running,
    Clean,
    Findings,
}

/// What a reviewer said of the PR as it is now, from its own posts on it
/// ([`super::reviews`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reviewer {
    /// What it last said of this head; None before it says anything.
    pub verdict: Option<Verdict>,
    /// When it said it, in Unix seconds (0 with nothing).
    pub at: u64,
    /// The body of the post its verdict came from, so a post rewritten
    /// since the decision changes its scope (`scope::current`).
    pub source: Option<String>,
    /// Its reviews of the PR that had findings, at any head: each a round.
    pub rounds: u32,
    /// Whether the PR was retargeted or its description edited at or after
    /// its head's push (or at all, when the push time isn't known): the
    /// reviewer's word binds only the commit, so it can't settle the PR
    /// alone.
    pub changed: bool,
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
    /// A judge's job, done, whose sealed issuance was read, or a fix's whose
    /// answers went up: its answer came, whether or not it still speaks for
    /// the revision.
    Answered,
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
    /// What it was judged on. Only a v3 issuance carries it.
    pub basis: Option<Basis>,
}

/// What a judgment was judged on: the reasons the flow asked for it, in the
/// contract's words, and the `flow::judging_context` of the discussion it
/// read. Sealed in the issuance and bound by its receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Basis {
    pub why: Vec<Trigger>,
    pub context: String,
}
impl Basis {
    /// As a judge request must say it: a nonempty, unique `why`, and a
    /// context that is a sha256 digest in lowercase hex.
    pub fn valid(&self) -> bool {
        !self.why.is_empty()
            && self
                .why
                .iter()
                .enumerate()
                .all(|(i, w)| !self.why[..i].contains(w))
            && self.context.len() == 64
            && self
                .context
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    }
}

/// Old saved issuances are plain verdicts. New ones retain their provenance,
/// and from v3 what they were judged on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JudgeResult {
    Recorded {
        verdict: Judgment,
        provenance: conary_core::plugin::Provenance,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        basis: Option<Basis>,
    },
    Legacy(Judgment),
}
impl Judged {
    pub(super) fn result(&self) -> JudgeResult {
        match &self.provenance {
            Some(provenance) => JudgeResult::Recorded {
                verdict: self.verdict,
                provenance: provenance.clone(),
                basis: self.basis.clone(),
            },
            None => JudgeResult::Legacy(self.verdict),
        }
    }
    pub(super) fn from_result(revision: Revision, result: JudgeResult) -> Self {
        match result {
            JudgeResult::Recorded {
                verdict,
                provenance,
                basis,
            } => Self {
                revision,
                verdict,
                provenance: Some(provenance),
                basis,
            },
            JudgeResult::Legacy(verdict) => Self {
                revision,
                verdict,
                provenance: None,
                basis: None,
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
                basis: None,
            }),
            _ => None,
        }
    }
    /// Whether this judgment still speaks for the PR: it was judged for
    /// every reason in `required` and on the discussion whose
    /// `flow::judging_context` is `context`. `asked` holds because a
    /// judgment already exists here, so no judgment must have been asked for
    /// it. An issuance that doesn't say what it was judged on never covers.
    pub fn covers(&self, required: &[Trigger], context: &str) -> bool {
        let Some(basis) = &self.basis else {
            return false;
        };
        context == basis.context
            && required
                .iter()
                .filter(|t| **t != Trigger::Asked)
                .all(|t| basis.why.contains(t))
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
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
pub enum Recipe {
    /// Builds this flake attribute.
    Build(String),
    /// Runs this program with these arguments.
    Run(Vec<String>),
    /// A check a model runs: no proof plugin is sent it, and it waits.
    Job(String),
}

/// Machine settings, never selected by the PR's author. The defaults name no
/// machine, account, label or engine, and start no paid work: an unset one
/// leaves its step waiting, as an omitted plugin does. Each field's doc is
/// its description in the published schema ([`super::schema`]), and each
/// says whether it starts paid work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    /// Whether PRs into the default branch land through the merge train.
    #[schemars(extend("x-spender" = true))]
    pub train: bool,
    /// The most changes one of the train's candidates holds.
    #[schemars(extend("x-spender" = false))]
    pub train_size: std::num::NonZeroUsize,
    /// The account a fix job pushes as. Set, a push to the branch by anyone
    /// else stops that PR's fix job.
    #[schemars(extend("x-spender" = false))]
    pub job_login: Option<String>,
    /// How many proofs may hold a slot at once. Zero, the default, runs
    /// none; `null` sets no limit.
    #[schemars(extend("x-spender" = true))]
    pub proof_slots: Option<u32>,
    /// Seconds a proof may run before it's stopped.
    #[schemars(extend("x-spender" = false))]
    pub proof_timeout: u64,
    /// Seconds a review, fix or judge job may run before it's stopped.
    #[schemars(extend("x-spender" = false))]
    pub job_timeout: u64,
    /// The author engines whose changes need a judge: not the engines that
    /// judge.
    #[schemars(extend("x-spender" = false))]
    pub judge_engines: BTreeSet<String>,
    /// The reviewers whose word a PR needs, by name. Each reads what the
    /// review authors post.
    #[schemars(extend("x-spender" = false))]
    pub reviewers: BTreeSet<String>,
    /// The accounts the reviewer posts as (Codex's GitHub connector): its
    /// summary comment and its reviews are read only from these. None set,
    /// no review counts.
    #[schemars(extend("x-spender" = false))]
    pub review_authors: BTreeSet<String>,
    /// The accounts the flow posts as: `review-settled`, the train's status
    /// and its answers to findings are read only from these. None set, none
    /// of them counts.
    #[schemars(extend("x-spender" = false))]
    pub flow_authors: BTreeSet<String>,
    /// The accounts whose sealed judge and dependency verdicts are read.
    #[schemars(extend("x-spender" = false))]
    pub verdict_authors: BTreeSet<String>,
    /// The key that seals a judge's verdict, as 64 lowercase hex digits.
    #[schemars(extend("x-spender" = false, "x-secret" = true))]
    pub judge_key: Option<SealKey>,
    /// A file holding `judge_key` instead, so the configuration needn't hold
    /// the secret: read each time the configuration loads, its surrounding
    /// whitespace ignored. Set this or `judge_key`, not both.
    #[schemars(extend("x-spender" = false))]
    pub judge_key_file: Option<std::path::PathBuf>,
    /// The key that seals a dependency sweep's verdict, as 64 lowercase hex
    /// digits.
    #[schemars(extend("x-spender" = false, "x-secret" = true))]
    pub major_key: Option<SealKey>,
    /// A file holding `major_key` instead, read as `judge_key_file` is. Set
    /// this or `major_key`, not both.
    #[schemars(extend("x-spender" = false))]
    pub major_key_file: Option<std::path::PathBuf>,
    /// Commit emails of the machine's own bots: a commit from one isn't a
    /// person's edit.
    #[schemars(extend("x-spender" = false))]
    pub bot_emails: BTreeSet<String>,
    /// Renovate's commit email. Unset, whether a person edited a Renovate PR
    /// is unknown.
    #[schemars(extend("x-spender" = false))]
    pub renovate_email: Option<String>,
    /// Renovate's login, whose comments say when a refresh may land.
    #[schemars(extend("x-spender" = false))]
    pub renovate_login: Option<String>,
    /// Unread: the label on an automerged upstream batch, from when Renovate
    /// merged its own PRs. Kept until burrow stops rendering it.
    #[schemars(extend("x-spender" = false))]
    pub batch_label: String,
    /// The label that holds a PR.
    #[schemars(extend("x-spender" = false))]
    pub hold_label: String,
    /// The priority level each label gives the PR it's on. A PR with labels
    /// of more than one level takes the highest.
    #[schemars(extend("x-spender" = false))]
    pub priority_labels: BTreeMap<String, Level>,
    /// The priority level of a PR with none of `priority_labels`.
    #[schemars(extend("x-spender" = false))]
    pub default_priority: Level,
    /// The label that hands a PR to its owner. None set, what the owner
    /// decides waits unlisted.
    #[schemars(extend("x-spender" = false))]
    pub owner_label: Option<String>,
    /// The people whose approving review is the owner's permission. None
    /// set, what the owner decides waits.
    #[schemars(extend("x-spender" = false))]
    pub people: BTreeSet<String>,
    /// Whether a PR needs green CI to land outside the train.
    #[schemars(extend("x-spender" = false))]
    pub require_ci: bool,
    /// The machine's proofs, by check name: what each one runs.
    #[schemars(extend("x-spender" = true))]
    pub proofs: BTreeMap<String, Recipe>,
    /// Seconds a PR must sit unchanged before its review is posted.
    #[schemars(extend("x-spender" = false))]
    pub settle_delay: u64,
    /// Seconds of quiet on a PR before a fix is handed out.
    #[schemars(extend("x-spender" = false))]
    pub handoff_after: u64,
    /// Seconds of quiet before a fix is handed out on a PR a session is
    /// working.
    #[schemars(extend("x-spender" = false))]
    pub session_handoff_after: u64,
    /// Review rounds before a PR that still has findings goes past the limit.
    #[schemars(extend("x-spender" = false))]
    pub rounds: u32,
    /// Seconds a PR may wait at one stage before it's overdue, by its
    /// priority level.
    #[schemars(extend("x-spender" = false))]
    pub stall_after: StallAfter,
    /// Seconds a held PR may wait before it's overdue.
    #[schemars(extend("x-spender" = false))]
    pub hold_stall_after: u64,
    /// Seconds a PR may wait in the train before it's overdue.
    #[schemars(extend("x-spender" = false))]
    pub train_stall_after: u64,
    /// Unread: seconds a Renovate PR could wait on its own automerge, from
    /// when Renovate merged its own PRs. Kept until burrow stops rendering it.
    #[schemars(extend("x-spender" = false))]
    pub renovate_merge_after: u64,
    /// Seconds Renovate is given to act after it's asked to rebase.
    #[schemars(extend("x-spender" = false))]
    pub renovate_run: u64,
    /// Whether dependency PRs wait on the machine's Renovate sweep.
    #[schemars(extend("x-spender" = true))]
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
    /// Each seal key given as a file (`judge_key_file`, `major_key_file`),
    /// read into its key. A key given both ways is refused, and so is a file
    /// that doesn't read or doesn't hold a key, without quoting what it holds.
    pub(crate) fn read_key_files(&mut self) -> Result<(), String> {
        for (name, key, file) in [
            ("judge_key", &mut self.judge_key, &self.judge_key_file),
            ("major_key", &mut self.major_key, &self.major_key_file),
        ] {
            let Some(file) = file else { continue };
            if key.is_some() {
                return Err(format!(
                    "settings.{name} and settings.{name}_file are both set: give one"
                ));
            }
            *key = Some(SealKey::read(file).map_err(|e| format!("settings.{name}_file: {e}"))?);
        }
        Ok(())
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            train: false,
            train_size: crate::train::DEFAULT_SIZE,
            job_login: None,
            proof_slots: Some(0),
            proof_timeout: 10800,
            job_timeout: 3600,
            judge_engines: BTreeSet::new(),
            reviewers: BTreeSet::from(["reviewer".into()]),
            review_authors: BTreeSet::new(),
            flow_authors: BTreeSet::new(),
            verdict_authors: BTreeSet::new(),
            judge_key: None,
            judge_key_file: None,
            major_key: None,
            major_key_file: None,
            bot_emails: BTreeSet::new(),
            renovate_email: None,
            renovate_login: None,
            batch_label: "upstream batch".into(),
            hold_label: "hold".into(),
            priority_labels: BTreeMap::from([
                ("priority: high".into(), Level::High),
                ("priority: medium".into(), Level::Medium),
                ("priority: low".into(), Level::Low),
            ]),
            default_priority: Level::Medium,
            owner_label: None,
            people: BTreeSet::new(),
            require_ci: true,
            proofs: BTreeMap::new(),
            settle_delay: 300,
            handoff_after: 1800,
            session_handoff_after: 7200,
            rounds: 5,
            stall_after: StallAfter::default(),
            hold_stall_after: 28800,
            train_stall_after: 7200,
            renovate_merge_after: 7200,
            renovate_run: 7200,
            renovate_sweep: false,
        }
    }
}

/// A review thread as GitHub shows it: whether it's resolved, the file and
/// line it's on (the line it was opened on, once outdated; none for a
/// thread on a whole file), and its comments in order. Its comments, not
/// whether it's resolved or where, are part of what a judge reads
/// ([`super::judging_context`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewThread {
    pub resolved: bool,
    pub path: String,
    pub line: Option<u64>,
    pub comments: Vec<ThreadComment>,
}

/// One comment on a review thread, or a review's body: who wrote it (`None`
/// for a deleted account) and what it says now, edits included.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ThreadComment {
    pub author: Option<String>,
    pub body: String,
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
    /// Opened by an App (a `Bot` account): the review connector reviews no
    /// head of such a PR unasked, so the flow asks at once.
    pub by_app: bool,
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
    /// The ids of this PR's workers that haven't answered: the work a pause
    /// or a hold stops.
    pub working: BTreeSet<String>,
    /// This PR's workers whose answers are there and don't read
    /// ([`super::degraded`]): each is taken as still running, and while
    /// any is, nothing is granted.
    pub unreadable: Vec<super::Degradation>,
    /// The repository's pause, when one covers this PR, read right before
    /// this decision.
    pub paused: Option<super::pause::Pause>,
    pub pushes: BTreeMap<String, String>,
    /// Every review thread on GitHub, by its id.
    pub threads: BTreeMap<String, ReviewThread>,
    /// The review bodies a judge reads ([`super::review_bodies`]), by review id.
    pub review_bodies: BTreeMap<u64, ThreadComment>,
    /// What each finished judge job on this revision was asked to judge on,
    /// by job id: `None` for a request from before the flow said.
    pub judge_requests: BTreeMap<String, Option<Basis>>,
    pub proof: Proof,
    pub proof_data: ProofData,
    pub settings: Settings,
    pub classified: Classified,
    pub authorship_known: bool,
    pub needs_judge: bool,
    pub judge_asked: bool,
    pub dependency: Option<Dependency>,
    pub renovate: bool,
    pub bot_edited: Option<bool>,
    pub gate_edited: bool,
    pub major: Option<Major>,
    pub release_at: Option<u64>,
    pub rebase_requested: bool,
    pub rebase_removed: Vec<u64>,
    pub timeline_cut_at: Option<u64>,
    pub stacked_on: Option<u64>,
    pub open_prs: BTreeSet<u64>,
    /// Where each open PR stands in line for a proof slot.
    pub queue: BTreeMap<u64, Rank>,
    /// Whether it's a fix for its repository's red default branch
    /// ([`super::red_fix`]).
    pub red_fix: bool,
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
    pub succeeded: BTreeSet<Failure>,
}
impl Facts {
    /// This PR's place in line, by its own labels and age.
    pub fn rank(&self) -> Rank {
        Rank::new(
            &self.settings,
            self.labels.iter().map(String::as_str),
            self.red_fix,
            self.created_at,
            self.number,
        )
    }
    /// [`super::judging_context`] over what this read saw on GitHub: what a
    /// judge asked now reads, and what a pass or owner verdict must have
    /// read to count. [`super::read_judging_context`] reads the same.
    pub fn context(&self) -> String {
        super::judging_context(&self.threads, &self.review_bodies)
    }
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
    /// Authenticated issuance high-water marks, including same-generation conflicts.
    #[serde(with = "super::persist::judge_issuance")]
    pub judge_issuance: JudgeIssuance,
    #[serde(with = "super::persist::pairs")]
    pub major_issuance: BTreeMap<Revision, (u64, Option<Major>)>,
    pub seen: u64,
    pub no_checks_since: Option<u64>,
    pub update_requested: Option<(String, u64)>,
    #[serde(with = "super::persist::pairs")]
    pub strikes: BTreeMap<Failure, u32>,
    pub retry: Option<(Failure, u64)>,
    /// The head the reviewer was last asked to look at, and when: asked
    /// again for the same head only an hour later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asked: Option<(String, u64)>,
    /// The head the reviewer was first asked to look at, and when: the clock
    /// [`super::review::LOST`] runs on, which asking again doesn't restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_asked: Option<(String, u64)>,
    #[serde(with = "super::persist::pairs")]
    pub handed: BTreeMap<Kind, Revision>,
    pub lost_jobs: BTreeSet<String>,
    #[serde(with = "super::persist::pairs")]
    pub unanswered: BTreeMap<(Kind, Revision), BTreeSet<String>>,
    #[serde(with = "super::persist::pairs")]
    pub broken_jobs: BTreeMap<String, u64>,
    pub answered_jobs: BTreeSet<String>,
    /// Judge jobs an authenticated issuance answered. Once answered, a job
    /// stays answered whatever is read later: a removed comment, a newer
    /// generation or a restart never makes it live or unanswered again.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub answered_judges: BTreeSet<String>,
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
    /// Ask the reviewer for a look at the head, saying why.
    AskReview(String),
    PostFix(String),
    HandFix(Kind),
    StopJob(String),
    /// Ready a proof's inputs, holding no proof slot.
    Prepare,
    Prove,
    PostProof,
    /// Ask a judge, for these reasons. Empty when a judge already started on
    /// this head and base is all that holds (the request then carries that
    /// judge's reasons), and in decisions saved before steps carried them.
    Judge(Vec<Trigger>),
    UpdateBranch,
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
    Train,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OwnerNotice {
    OpenOverDay,
    /// No review of its head came hours after its push.
    NoReviewer,
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
    #[serde(default, deserialize_with = "super::persist::proof_plan::deserialize")]
    pub proof: Option<ProofPlan>,
    pub wait_on: Option<WaitOn>,
    pub on_usage: bool,
    /// A digest decision, independent of the one executable intention.
    pub owner_notice: Option<OwnerNotice>,
    /// Every switch in effect that stops this PR, whichever of them decided
    /// its step, and what each one holds ([`super::switches`]). Left out
    /// when there are none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub switches: Vec<super::Holding>,
    /// The PR's priority level, which orders its proof slot and train
    /// admission and sizes its stall deadline.
    #[serde(default)]
    pub priority: Level,
    /// Every path that half-worked for this PR, this pass or as the ledger
    /// still holds it for its revision ([`super::degraded`]): what came
    /// through one counts as its check's failure. Left out when there are
    /// none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub degraded: Vec<super::Degradation>,
}
