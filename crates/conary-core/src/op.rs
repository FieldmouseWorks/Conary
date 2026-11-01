//! What the op log records (`docs/DESIGN.md` section 4.6): one operation per
//! write to the ledger, each naming the one before it.

use serde::{Deserialize, Serialize};

use crate::id::{CandidateId, ChangeId, CommitId, OpId, RevisionId, TreeId};

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
    /// landing until it carries its producer's attestation (design 4.3).
    Evidence {
        change: ChangeId,
        revision: RevisionId,
        kind: CheckKind,
        check: String,
        result: Outcome,
        /// What produced it: a check run's URL, a reviewer's name.
        source: String,
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
    /// bytes, ids and signatures are the same either way.
    Join {
        change: ChangeId,
        revision: RevisionId,
        #[serde(default, skip_serializing_if = "is_false")]
        alone: bool,
    },
    /// The change left the train without landing.
    Leave { change: ChangeId, why: Left },
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
    /// revision's evidence, and counted only as that is (design 4.3).
    CandidateEvidence {
        candidate: CandidateId,
        kind: CheckKind,
        check: String,
        result: Outcome,
        source: String,
    },
    /// How the candidate's gate ended. Until Conary counts attested
    /// evidence itself, this is the steward's call from the candidate's CI,
    /// as it reads a PR's.
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
}

impl Body {
    /// The change an op is about, if it's about one.
    pub fn change(&self) -> Option<ChangeId> {
        match self {
            Self::Observe { change, .. }
            | Self::Intent { change, .. }
            | Self::Revision { change, .. }
            | Self::Evidence { change, .. }
            | Self::Land { change, .. }
            | Self::Close { change }
            | Self::Reopen { change }
            | Self::Join { change, .. }
            | Self::Leave { change, .. } => Some(*change),
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
