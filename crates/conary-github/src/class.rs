//! A change's class, and the evidence it needs (`docs/DESIGN.md` section
//! 4.4): what the machine's policy says of the files a PR's revision
//! touches, recorded in the ledger as a `classify` op.

use conary_core::ledger::Classed;
use conary_core::op::{Body, Repo};
use conary_core::policy::{Class, Classified, Policy};
use conary_core::store::StoreError;
use conary_core::{ChangeId, RevisionId, Store};

use crate::api::{ApiError, Source};
use crate::ingest::{self, Config, IngestError};

#[derive(Debug)]
pub enum ClassError {
    Api(ApiError),
    Ingest(IngestError),
    Store(StoreError),
    /// The PR's head isn't the one the caller asked about.
    Moved {
        pr: u64,
        head: String,
    },
}

impl std::fmt::Display for ClassError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Api(e) => e.fmt(f),
            Self::Ingest(e) => e.fmt(f),
            Self::Store(e) => e.fmt(f),
            Self::Moved { pr, head } => write!(f, "#{pr}'s head is {head} now"),
        }
    }
}

impl std::error::Error for ClassError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Api(e) => Some(e),
            Self::Ingest(e) => Some(e),
            Self::Store(e) => Some(e),
            Self::Moved { .. } => None,
        }
    }
}

impl From<ApiError> for ClassError {
    fn from(e: ApiError) -> Self {
        Self::Api(e)
    }
}

impl From<IngestError> for ClassError {
    fn from(e: IngestError) -> Self {
        Self::Ingest(e)
    }
}

impl From<StoreError> for ClassError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

/// A revision's class, as the ledger holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classes {
    pub change: ChangeId,
    pub revision: RevisionId,
    pub said: Classified,
    /// Whether this call recorded it, rather than finding it recorded.
    pub recorded: bool,
}

impl Classes {
    pub fn class(&self) -> Class {
        self.said.class
    }
}

fn said(k: &Classed) -> Classified {
    Classified {
        class: k.class,
        because: k.because.clone(),
        docs: k.docs,
        raised: k.raised.clone(),
    }
}

/// The class of a PR at `head`, once the ledger holds the PR as GitHub
/// shows it now and that is its head ([`of`]), raised by the PR's labels as
/// they are now. Asked again, it reads GitHub for the PR and nothing more.
pub fn class(
    store: &mut Store,
    source: &dyn Source,
    config: &Config,
    policy: &Policy,
    repo: &Repo,
    pr: u64,
    head: &str,
) -> Result<Classes, ClassError> {
    let (got, pull) = ingest::pulled(store, source, config, repo, pr)?;
    let c = store
        .ledger()
        .change(got.change)
        .expect("ingest recorded it");
    let now = &c.revision(got.revision).expect("ingest recorded it").head;
    if now.as_str() != head {
        return Err(ClassError::Moved {
            pr,
            head: now.to_string(),
        });
    }
    of(
        store,
        source,
        policy,
        repo,
        got.change,
        got.revision,
        pull.raises(),
    )
}

/// The class of a revision the ledger holds, raised by its PR's labels
/// (`raised`, [`PullRequest::raises`](crate::api::PullRequest::raises)).
/// The ledger's word for it under this policy and these labels is the
/// answer when it has one; otherwise the files the revision changes from
/// its merge base are read, and what the policy says of them, raised, is
/// recorded.
pub(crate) fn of(
    store: &mut Store,
    source: &dyn Source,
    policy: &Policy,
    repo: &Repo,
    change: ChangeId,
    revision: RevisionId,
    raised: Vec<String>,
) -> Result<Classes, ClassError> {
    let c = store.ledger().change(change).expect("the caller's change");
    if let Some(k) = c.class_of(revision)
        && k.policy == policy.digest()
        && k.raised == raised
    {
        return Ok(Classes {
            change,
            revision,
            said: said(k),
            recorded: false,
        });
    }
    let r = c.revision(revision).expect("the caller's revision");
    let paths = source.files(&repo.to_string(), r.base.as_str(), r.head.as_str())?;
    let said = policy.classify(repo, &paths).raise(raised);
    store.record(Body::Classify {
        change,
        revision,
        class: said.class,
        because: said.because.clone(),
        docs: said.docs,
        policy: policy.digest().to_owned(),
        raised: said.raised.clone(),
    })?;
    Ok(Classes {
        change,
        revision,
        said,
        recorded: true,
    })
}

#[cfg(test)]
mod tests;
