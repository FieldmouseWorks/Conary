//! A change's class, and the evidence it needs (`docs/DESIGN.md` section
//! 4.4): what the machine's policy says of the files a PR's revision
//! touches, recorded in the ledger as a `classify` op.

use std::path::Path;
use std::sync::Arc;

use conary_core::ledger::{Change, Classed, Ledger};
use conary_core::op::{Body, Repo};
use conary_core::policy::{Class, Classified, Paths, Policy, raises};
use conary_core::reader::{ReadError, Reader};
use conary_core::store::StoreError;
use conary_core::{ChangeId, CommitId, OpId, RevisionId, Store};

use crate::api::{ApiError, PullRequest, Source};
use crate::bases::MergeBases;
use crate::ingest::{self, Config, IngestError};

#[derive(Debug)]
pub enum ClassError {
    Api(ApiError),
    Ingest(IngestError),
    Store(StoreError),
    Read(ReadError),
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
            Self::Read(e) => e.fmt(f),
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
            Self::Read(e) => Some(e),
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

impl From<ReadError> for ClassError {
    fn from(e: ReadError) -> Self {
        Self::Read(e)
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

/// The ledger as [`ask`] reads it, without the lock: as it was read before
/// GitHub was, and as it is now.
pub trait Unlocked {
    fn before(&self) -> Arc<Ledger>;
    fn now(&mut self) -> Result<Arc<Ledger>, ReadError>;
}

impl Unlocked for Reader {
    fn before(&self) -> Arc<Ledger> {
        self.ledger()
    }

    /// Read again only if the log moved since.
    fn now(&mut self) -> Result<Arc<Ledger>, ReadError> {
        self.refresh()
    }
}

/// What [`ask`] found before the ledger's lock: the class itself, when the
/// ledger already held it, or what [`class`] records under the lock.
pub enum Asked {
    Held(Classes),
    Read(Box<Read>),
}

/// A PR as [`ask`] read it from GitHub, and the last op the ledger held
/// about its change before that.
pub struct Read {
    config: Config,
    repo: Repo,
    head: String,
    pr: PullRequest,
    base: CommitId,
    read: ingest::Read,
    bases: MergeBases,
    /// The PR's revision as read, and the files it changes from its merge
    /// base, unless the ledger already held its class under this policy.
    revision: RevisionId,
    paths: Option<Paths>,
    /// `None` when the ledger held no change for the PR.
    last: Option<OpId>,
}

/// Reads what [`class`] needs of a PR at `head` before the ledger's lock
/// is taken, so other writers wait only for the recording: the PR, its
/// merge base, its results and timeline, and the files its revision
/// changes. `ledger` is the ledger before any of it. When the ledger holds
/// the PR at that head with its class under `policy`, raised by the PR's
/// labels as they are now, that's the answer and there is nothing to
/// record: the PR's other news since is left to a sync. That's asked of
/// the ledger as it is after the reads: another writer may have recorded
/// the PR meanwhile, a raise by a label taken off since too, and a raise
/// stays once it's recorded. The merge base comes from the cache beside the
/// ledger in `dir`, read as it is; one it doesn't hold is asked of GitHub,
/// and [`class`] keeps it there.
#[allow(clippy::too_many_arguments)]
pub fn ask(
    ledger: &mut dyn Unlocked,
    source: &dyn Source,
    config: &Config,
    policy: &Policy,
    repo: &Repo,
    pr: u64,
    head: &str,
    dir: &Path,
) -> Result<Asked, ClassError> {
    config.check()?;
    let full = repo.to_string();
    let before = ledger.before();
    let held = before.by_pr(repo, pr);
    let pr = source.pull(&full, pr)?;
    let mut bases = MergeBases::read(dir).map_err(IngestError::Cache)?;
    let base = ingest::commit(
        &bases
            .get(source, &full, &pr.base.sha, &pr.head.sha)
            .map_err(IngestError::from)?,
    )?;
    let revision = RevisionId::new(&base, &ingest::commit(&pr.head.sha)?);
    let moved = pr.head.sha != head;
    let classed = held.and_then(|c| answer(c, revision, policy, pr.raises()));
    let current = |c: &Change| c.current == Some(revision);
    if !moved && classed.is_some() && held.is_some_and(current) {
        let now = ledger.now()?;
        if let Some(got) = now
            .by_pr(repo, pr.number)
            .filter(|c| current(c))
            .and_then(|c| answer(c, revision, policy, pr.raises()))
        {
            return Ok(Asked::Held(got));
        }
    }
    // A head that moved is refused once the PR is recorded, unclassed.
    let paths = if moved || classed.is_some() {
        None
    } else {
        Some(source.files(&full, base.as_str(), &pr.head.sha)?)
    };
    let read = ingest::Read::of(source, &full, &pr)?;
    Ok(Asked::Read(Box::new(Read {
        config: config.clone(),
        repo: repo.clone(),
        head: head.to_owned(),
        pr,
        base,
        read,
        bases,
        revision,
        paths,
        last: held.and_then(Change::last),
    })))
}

/// The class of a PR at `head`, once the ledger holds the PR as GitHub
/// shows it now and that is its head ([`of`]), raised by the PR's labels as
/// they are now, from what [`ask`] read. While the last op the ledger
/// holds about the PR's change is the one it held before that read, no
/// other writer can have recorded the PR as it was since, so what was read
/// is recorded with no call to GitHub; otherwise, even where what was
/// recorded since set the change back as it was, the PR is read again
/// here, under the lock, as [`ingest::pull`] does.
pub fn class(
    store: &mut Store,
    source: &dyn Source,
    policy: &Policy,
    read: Read,
) -> Result<Classes, ClassError> {
    let Read {
        config,
        repo,
        head,
        pr,
        base,
        read,
        bases: asked,
        revision,
        paths,
        last,
    } = read;
    let number = pr.number;
    MergeBases::load(store.lock())
        .and_then(|mut bases| bases.keep(asked))
        .map_err(IngestError::Cache)?;
    let (got, pr) = if store.ledger().by_pr(&repo, number).and_then(Change::last) == last {
        let got = ingest::record(store, source, &config, &repo, &pr, base, read)?;
        (got, pr)
    } else {
        ingest::pulled(store, source, &config, &repo, number)?
    };
    // The files a revision changes never change.
    let paths = paths.filter(|_| got.revision == revision);
    let c = store
        .ledger()
        .change(got.change)
        .expect("ingest recorded it");
    let now = &c.revision(got.revision).expect("ingest recorded it").head;
    if now.as_str() != head {
        return Err(ClassError::Moved {
            pr: number,
            head: now.to_string(),
        });
    }
    classify(
        store,
        policy,
        &repo,
        files(source, &repo, paths),
        got.change,
        got.revision,
        pr.raises(),
    )
}

/// [`ask`] on the ledger `store` holds, then [`class`] when it didn't hold
/// the class: `cn class` either side of taking the lock, as one call.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn at_once(
    store: &mut Store,
    source: &dyn Source,
    config: &Config,
    policy: &Policy,
    repo: &Repo,
    pr: u64,
    head: &str,
) -> Result<Classes, ClassError> {
    let dir = store.lock().dir().to_owned();
    let ledger = Arc::new(store.ledger().clone());
    let mut given = Given {
        before: Arc::clone(&ledger),
        now: ledger,
    };
    match ask(&mut given, source, config, policy, repo, pr, head, &dir)? {
        Asked::Held(got) => Ok(got),
        Asked::Read(read) => class(store, source, policy, *read),
    }
}

/// The ledger as a test gives it to [`ask`]: as it was before the reads,
/// and as it is after them.
#[cfg(test)]
pub(crate) struct Given {
    pub before: Arc<Ledger>,
    pub now: Arc<Ledger>,
}

#[cfg(test)]
impl Unlocked for Given {
    fn before(&self) -> Arc<Ledger> {
        Arc::clone(&self.before)
    }

    fn now(&mut self) -> Result<Arc<Ledger>, ReadError> {
        Ok(Arc::clone(&self.now))
    }
}

/// The ledger's word on a revision of `c` under `policy`, raised by
/// `raised` and every label that raised the change before, if it has one.
fn answer(
    c: &Change,
    revision: RevisionId,
    policy: &Policy,
    raised: Vec<String>,
) -> Option<Classes> {
    let raised = raises(c.raised().iter().chain(&raised).map(String::as_str));
    c.class_of(revision)
        .filter(|k| k.policy == policy.digest() && k.raised == raised)
        .map(|k| Classes {
            change: c.id,
            revision,
            said: said(k),
            recorded: false,
        })
}

/// The files a revision changes: `read` before, or read from GitHub now.
fn files<'a>(
    source: &'a dyn Source,
    repo: &'a Repo,
    read: Option<Paths>,
) -> impl FnOnce(&str, &str) -> Result<Paths, ApiError> + 'a {
    move |base, head| match read {
        Some(paths) => Ok(paths),
        None => source.files(&repo.to_string(), base, head),
    }
}

/// The class of a revision the ledger holds, raised by its PR's labels
/// (`raised`, [`PullRequest::raises`](crate::api::PullRequest::raises))
/// and by every label that raised the change before: a raise stays once
/// it's recorded. The ledger's word for it under this policy and these
/// raises is the answer when it has one; otherwise the files the revision
/// changes from its merge base are read, and what the policy says of them,
/// raised, is recorded.
pub(crate) fn of(
    store: &mut Store,
    source: &dyn Source,
    policy: &Policy,
    repo: &Repo,
    change: ChangeId,
    revision: RevisionId,
    raised: Vec<String>,
) -> Result<Classes, ClassError> {
    classify(
        store,
        policy,
        repo,
        files(source, repo, None),
        change,
        revision,
        raised,
    )
}

/// [`of`], with the revision's files from `files` when they're needed.
fn classify(
    store: &mut Store,
    policy: &Policy,
    repo: &Repo,
    files: impl FnOnce(&str, &str) -> Result<Paths, ApiError>,
    change: ChangeId,
    revision: RevisionId,
    raised: Vec<String>,
) -> Result<Classes, ClassError> {
    let c = store.ledger().change(change).expect("the caller's change");
    if let Some(held) = answer(c, revision, policy, raised.clone()) {
        return Ok(held);
    }
    let raised = raises(c.raised().iter().chain(&raised).map(String::as_str));
    let r = c.revision(revision).expect("the caller's revision");
    let paths = files(r.base.as_str(), r.head.as_str())?;
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
