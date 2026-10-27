//! What the op log records (`docs/DESIGN.md` section 4.6): one operation per
//! write to the ledger, each naming the one before it.

use serde::{Deserialize, Serialize};

use crate::id::{ChangeId, CommitId, OpId, RevisionId};

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
            | Self::Reopen { change } => Some(*change),
            Self::Outside { .. } => None,
        }
    }
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
