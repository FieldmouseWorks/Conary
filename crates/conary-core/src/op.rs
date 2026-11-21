//! What the op log records (`docs/DESIGN.md` section 4.6): one operation per
//! write to the ledger, each naming the one before it.

use serde::{Deserialize, Serialize};

use crate::id::{CandidateId, ChangeId, CommitId, OpId, RevisionId, TreeId};
use crate::policy::Class;

/// A GitHub repository, as `owner/name`. GitHub ignores case in both, so two
/// spellings of one repository are equal: an op keeps the spelling it was
/// recorded with.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repo {
    pub owner: String,
    pub name: String,
}

impl Repo {
    fn key(&self) -> (String, String) {
        (
            self.owner.to_ascii_lowercase(),
            self.name.to_ascii_lowercase(),
        )
    }
}

impl PartialEq for Repo {
    fn eq(&self, other: &Self) -> bool {
        self.owner.eq_ignore_ascii_case(&other.owner) && self.name.eq_ignore_ascii_case(&other.name)
    }
}

impl Eq for Repo {}

impl std::hash::Hash for Repo {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.key().hash(state);
    }
}

impl PartialOrd for Repo {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Repo {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.key().cmp(&other.key())
    }
}

impl std::str::FromStr for Repo {
    type Err = String;

    /// `owner/name`.
    fn from_str(s: &str) -> Result<Self, String> {
        match s.split_once('/') {
            Some((owner, name)) if !owner.is_empty() && !name.is_empty() && !name.contains('/') => {
                Ok(Self {
                    owner: owner.to_owned(),
                    name: name.to_owned(),
                })
            }
            _ => Err(format!("{s:?} isn't owner/name")),
        }
    }
}

impl std::fmt::Display for Repo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

/// What kind of actor made a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    Agent,
    Person,
    Bot,
}

/// Who made a change: a login, and the agent session behind it when there
/// is one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    pub kind: ActorKind,
    pub login: String,
    pub session: Option<String>,
}

/// What a check is, as the design's evidence record names its kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckKind {
    Ci,
    Review,
    Proof,
    Judge,
    Negative,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Pass,
    Fail,
    Error,
    /// Nothing to run: a `negative` check's change adds no test to code
    /// (`docs/ci-attestation.md`, "Negative runs"), so it needs none.
    None,
}

/// What a landing's authorization rests on, as strong as it is and no more.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Authority {
    /// The steward's settled status, as GitHub shows it from `login` before
    /// the merge. Unattested: anyone holding that login's token can post one,
    /// so it says what GitHub shows, never that the steward decided it.
    Status { login: String },
}

/// One change in a train candidate, at the revision it joined at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub change: ChangeId,
    pub revision: RevisionId,
}

/// Why a change left its repo's train without landing, when nothing else in
/// the ledger says so. A new head, a close and a landing take a change out
/// by themselves, as does its candidate going red with it alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Left {
    /// It no longer can land but for the gate: a thread opened again, say.
    Unready,
    /// Its head won't merge onto trunk and the changes ahead of it.
    Conflict,
    /// Its head merges onto trunk and the changes ahead of it without
    /// changing their tree: what it does is there already, so landing it
    /// would only add an empty merge.
    Empty,
    /// GitHub refused its merge.
    Refused,
    /// It's in below its revision's class (`docs/DESIGN.md` section 4.4),
    /// classed R2 after it joined: it's back in at once, alone, at the back,
    /// at the same revision.
    Below,
}

/// A producer's own word that a result came from its run on what it ran
/// on, kept as it was checked (`docs/ci-attestation.md`,
/// `docs/service-attestation.md`), so it can be checked again for as long
/// as its signer's key is published or trusted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Attest {
    /// A private repo's CI: the statement its job attested, and the job's
    /// GitHub OIDC token, whose audience names the statement's digest.
    GithubToken { statement: String, token: String },
    /// A service on the machine (a reviewer, a proof, judge or negative
    /// job): the statement it signed, the Ed25519 public key it signed
    /// with and the signature over the statement's bytes, both base64url.
    Service {
        statement: String,
        key: String,
        signature: String,
    },
}

/// How a candidate's gate ended (`docs/DESIGN.md` section 4.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Its changes land, in order.
    Green,
    /// It splits in two, older half first, or its one change leaves.
    Red,
    /// Its gate never finished: its changes wait in the train again, as
    /// they joined.
    Dropped,
}

/// What asked for an undo (`docs/DESIGN.md` section 5), which is what its
/// revert reverts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "by", rename_all = "snake_case")]
pub enum Asked {
    /// One change.
    Change,
    /// Every change one agent session landed, in its repo.
    Actor { session: String },
    /// Everything that landed on its repo's trunk after `since`
    /// (milliseconds since the Unix epoch, as the landings' ops say it).
    Since { since: i64 },
}

/// What half-worked (`docs/DESIGN.md` section 4.6): a path that went on
/// without the answer it was for. Whatever came through it counts as its
/// check's failure until a later [`Body::Restored`] says it works again.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "path", rename_all = "snake_case")]
pub enum Degraded {
    /// The flow's record of the change's work didn't read. Its workers were
    /// taken as holding every proof slot, and none of its answers count.
    Record,
    /// The answer of the flow's worker `job`, doing `kind` of work, didn't
    /// read, or isn't an answer to its kind of work, or the worker's stop
    /// marker doesn't say why it was stopped: it was taken as still
    /// running, holding its proof slot if it's a proof, and counts as no
    /// answer.
    Answer {
        job: String,
        kind: crate::plugin::Kind,
    },
    /// A lookup of `check`'s attested result couldn't tell whether it holds:
    /// GitHub or the producer's attestation didn't answer.
    Lookup { check: String },
    /// A read of GitHub (or of the repository's pause) the flow's decision
    /// for the change needed failed or stopped short of all it holds. What
    /// it was for counts as failed: a check that can't be read is pending,
    /// provenance that can't be read is unknown, and a decision whose reads
    /// failed outright waits.
    Read { read: Reading },
    /// A mark one of the accounts the flow trusts for it left on the PR
    /// doesn't parse: the PR waits on it, and what it would have said
    /// counts for nothing.
    Mark { mark: Mark },
}

/// Which mark a [`Degraded::Mark`] names: a closed set, one per mark the
/// flow's decision reads from the PR's comments (`docs/flow.md`, "Degraded
/// paths").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mark {
    /// A judge's or the dependency sweep's sealed verdict.
    Verdict,
    /// The reviewer's summary of its review of the head.
    Review,
    /// Renovate's word that nothing in a lock refresh is old enough yet.
    ReleaseAge,
}

impl std::fmt::Display for Mark {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Verdict => "a sealed verdict",
            Self::Review => "the reviewer's summary",
            Self::ReleaseAge => "Renovate's release-age wait",
        })
    }
}

/// Which read a [`Degraded::Read`] names: a closed set, one per read the
/// flow's decision takes (`docs/flow.md`, "Degraded paths").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reading {
    /// The PR itself: its REST read, or its GraphQL snapshot.
    Pull,
    /// The PR's GraphQL snapshot, which stopped short of its reviews,
    /// comments, labels or threads.
    Snapshot,
    /// The repository's open PRs.
    Pulls,
    /// The base branch's tip.
    Base,
    /// The files the PR changes.
    Files,
    /// The head's check runs, suites and statuses.
    Checks,
    /// The base tip and merge base a docs lane's word is checked against.
    Fork,
    /// What became of a cancelled run: its suite, or its annotations.
    Cancelled,
    /// The pushes to the PR's branch.
    Pushes,
    /// The PR's reviews and comments.
    Reviews,
    /// The PR's commits, compared with its base.
    Commits,
    /// A commit's trailers, as git parses them.
    Trailers,
    /// GitHub's test merge: its commit and its tree beside the base tip's.
    MergeTree,
    /// The PR's diff, for the gates it edits.
    Diff,
    /// The default branch's CI, as the snapshot observes it.
    DefaultBranch,
    /// The head's checks, read again before permission is granted.
    Observations,
    /// The repository's pause.
    Pause,
}

impl std::fmt::Display for Reading {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Pull => "the PR",
            Self::Snapshot => "the PR's snapshot",
            Self::Pulls => "the open PRs",
            Self::Base => "the base branch",
            Self::Files => "the PR's files",
            Self::Checks => "the head's checks",
            Self::Fork => "the docs lane's fork point",
            Self::Cancelled => "a cancelled run",
            Self::Pushes => "the branch's pushes",
            Self::Reviews => "the PR's reviews",
            Self::Commits => "the PR's commits",
            Self::Trailers => "a commit's trailers",
            Self::MergeTree => "GitHub's test merge",
            Self::Diff => "the PR's diff",
            Self::DefaultBranch => "the default branch's CI",
            Self::Observations => "the head's checks before permission",
            Self::Pause => "the repository's pause",
        })
    }
}

impl std::fmt::Display for Degraded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Record => f.write_str("its flow record didn't read"),
            Self::Answer { job, kind } => {
                let kind = serde_json::to_value(kind).map_err(|_| std::fmt::Error)?;
                write!(
                    f,
                    "{} worker {job}'s answer didn't read",
                    kind.as_str().unwrap_or("a")
                )
            }
            Self::Lookup { check } => write!(f, "{check}'s evidence lookup couldn't tell"),
            Self::Read { read } => write!(f, "the read of {read} failed or stopped short"),
            Self::Mark { mark } => write!(f, "{mark}'s mark didn't parse"),
        }
    }
}

/// The most bytes of a degradation's `why`: it's permanent, so it says
/// what failed in a line, never a body read back from GitHub or a plugin.
pub const DEGRADED_WHY: usize = 200;

/// A new undo's operational reason, preserved verbatim. Callers must keep
/// secrets, personal data and prompt text out of this permanent record.
pub fn undo_reason(reason: &str) -> Result<String, &'static str> {
    if reason.trim().is_empty() {
        Err("an undo requires a nonblank reason")
    } else {
        Ok(reason.to_owned())
    }
}

/// The body of an operation: what happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Body {
    /// Conary saw a change for the first time and gave it its id.
    Observe {
        change: ChangeId,
        repo: Repo,
        pr: u64,
        author: Actor,
    },
    /// Why the change exists. A later intent op supersedes an earlier one.
    Intent {
        change: ChangeId,
        summary: String,
        body: String,
        /// The issue or thread the work came from, when one is linked.
        task: Option<String>,
    },
    /// A head of the change on a base, as Conary saw it.
    Revision {
        change: ChangeId,
        base: CommitId,
        head: CommitId,
    },
    /// One result of one check on one revision. Recorded, never counted for
    /// landing until it carries its producer's attestation (design 4.3). A
    /// result without one is written without the field, as results were
    /// before it, so their bytes, ids and signatures are the same either way.
    Evidence {
        change: ChangeId,
        revision: RevisionId,
        kind: CheckKind,
        check: String,
        result: Outcome,
        /// What produced it: a check run's URL, a reviewer's name.
        source: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attest: Option<Attest>,
    },
    /// What the class policy said of one revision of the change (design
    /// 4.4): its class, what made it R2, and whether every path it touches
    /// is documentation. `policy` is the BLAKE3 hash of the policy file
    /// that said it, and `raised` the labels its PR raised it with. A later
    /// one for the same revision supersedes it.
    Classify {
        change: ChangeId,
        revision: RevisionId,
        class: Class,
        because: Option<String>,
        docs: bool,
        policy: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        raised: Vec<String>,
    },
    /// Trunk moved to `after` by landing this revision of the change.
    Land {
        change: ChangeId,
        revision: RevisionId,
        before: CommitId,
        after: CommitId,
        /// What says it landed through `land`.
        by: Authority,
    },
    /// Trunk moved some other way. A violation, listed for the owner.
    Outside {
        repo: Repo,
        before: Option<CommitId>,
        after: CommitId,
    },
    /// The change was closed without landing.
    Close { change: ChangeId },
    /// A closed change was opened again.
    Reopen { change: ChangeId },
    /// The change joined its repo's train: everything but the gate says
    /// this revision can land, the PR's head as it is now. Joining at
    /// another revision than the one it's in at takes it out and puts it
    /// at the back. An `alone` change rides in a candidate of its own and
    /// lands only while trunk is still that candidate's base: an R2 change's
    /// way (design 4.4). Until classes are the ledger's, the steward says
    /// which changes those are, from its one-way paths. A join that isn't
    /// alone is written without the field, as joins were before it, so their
    /// bytes, ids and signatures are the same either way. `raised` is every
    /// label recorded as raising the change when it joined ([`Classify`]'s,
    /// at any revision): what the steward settled it with. A raise recorded
    /// since takes it out before it lands, to be settled again. Written
    /// without the field when there are none, as `alone` is.
    ///
    /// [`Classify`]: Body::Classify
    Join {
        change: ChangeId,
        revision: RevisionId,
        #[serde(default, skip_serializing_if = "is_false")]
        alone: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        raised: Vec<String>,
    },
    /// The change left the train without landing. `trunk`, for an empty
    /// merge with nothing merged before it, is the trunk commit its
    /// candidate was built on: one that already does what it does.
    Leave {
        change: ChangeId,
        why: Left,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trunk: Option<CommitId>,
    },
    /// A train candidate, whole: `trunk`, the branch, at `base`, then each
    /// change's head merged onto it in order, ending at `commit`, on the
    /// candidate's train branch, where the gate runs. It lands onto that
    /// branch alone. `of` is the red candidate it's a half of.
    Candidate {
        candidate: CandidateId,
        repo: Repo,
        trunk: String,
        base: CommitId,
        changes: Vec<Member>,
        commit: CommitId,
        tree: TreeId,
        of: Option<CandidateId>,
    },
    /// One result of one check on a candidate's commit. Recorded like a
    /// revision's evidence, and counted only as that is (design 4.3): by
    /// its producer's attestation, checked when it was recorded.
    CandidateEvidence {
        candidate: CandidateId,
        kind: CheckKind,
        check: String,
        result: Outcome,
        source: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attest: Option<Attest>,
    },
    /// How the candidate's gate ended: green on its attested evidence
    /// (design 4.7).
    Verdict {
        candidate: CandidateId,
        verdict: Verdict,
    },
    /// Every change in a green candidate landed, and trunk ended on `tree`,
    /// the last landing's. When that's the candidate's tree, the
    /// candidate's evidence is about that landing's tree.
    Finish {
        candidate: CandidateId,
        tree: TreeId,
    },
    /// A path that half-worked for this revision of the change: what
    /// degraded and why (at most [`DEGRADED_WHY`] bytes). Recorded once while
    /// it lasts, and never removed: a later [`Body::Restored`] clears it.
    Degraded {
        change: ChangeId,
        revision: RevisionId,
        what: Degraded,
        why: String,
    },
    /// The path a [`Body::Degraded`] named works again for that revision.
    Restored {
        change: ChangeId,
        revision: RevisionId,
        what: Degraded,
    },
    /// `change` is an undo: its PR reverts `reverts`, landed changes in its
    /// repo, because `asked` asked for them. Once it lands they're
    /// reverted, and they're landed again if it's undone in turn.
    Undo {
        change: ChangeId,
        reverts: Vec<ChangeId>,
        asked: Asked,
        /// Why the undo was requested. Absent only in historical records:
        /// omitting it keeps their signed bytes and ids unchanged.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

impl Body {
    /// The change an op is about, if it's about one.
    pub fn change(&self) -> Option<ChangeId> {
        match self {
            Self::Observe { change, .. }
            | Self::Intent { change, .. }
            | Self::Revision { change, .. }
            | Self::Evidence { change, .. }
            | Self::Classify { change, .. }
            | Self::Land { change, .. }
            | Self::Close { change }
            | Self::Reopen { change }
            | Self::Join { change, .. }
            | Self::Leave { change, .. }
            | Self::Degraded { change, .. }
            | Self::Restored { change, .. }
            | Self::Undo { change, .. } => Some(*change),
            Self::Outside { .. }
            | Self::Candidate { .. }
            | Self::CandidateEvidence { .. }
            | Self::Verdict { .. }
            | Self::Finish { .. } => None,
        }
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// The part of an op its id hashes and its signature covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unsigned {
    pub parent: Option<OpId>,
    /// Milliseconds since the Unix epoch.
    pub at: i64,
    pub body: Body,
}

/// One operation, as replayed from the op log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Op {
    pub id: OpId,
    pub op: Unsigned,
}
