//! A sync: the ledger brought up to date with a repo, read in two parts so
//! the ledger's writer lock is held only while it records. [`gather`] reads
//! what takes minutes, each open PR's check runs, statuses and reviews,
//! before the lock; [`sync`], under it, lists the PRs again and records
//! from what was gathered, with each PR's timeline read again, reading
//! everything again only for a PR whose revision changed since.

use std::collections::HashMap;
use std::path::Path;

use conary_core::ledger::{Change, Ledger, Status};
use conary_core::op::Repo;
use conary_core::{RevisionId, Store};

use super::{Config, IngestError, Ingested, Read, Results, commit, record};
use crate::api::{PullRequest, Pulls, Source};
use crate::bases::MergeBases;

/// What a sync read of a repo's open PRs before it took the ledger's lock.
pub struct Gathered {
    config: Config,
    repo: Repo,
    bases: MergeBases,
    open: HashMap<u64, Held>,
}

/// One open PR's results, and the revision it was at when they were read.
struct Held {
    revision: RevisionId,
    results: Results,
}

/// Reads what a [`sync`] of `repo` records for its open PRs, without the
/// ledger: run it before the lock is taken, so other writers wait only for
/// the recording. A PR that can't be read now is read again under the lock,
/// where a failure stops the sync once the PRs before it are recorded, as
/// it would have with nothing gathered; so are the ones after it.
///
/// Each PR's merge base, what tells [`sync`] whether the results are still
/// the revision's it records, comes from the cache beside the ledger in
/// `dir`, read as it is; one it doesn't hold is asked of GitHub, and [`sync`]
/// keeps it there.
pub fn gather(
    source: &dyn Source,
    config: &Config,
    repo: &Repo,
    dir: &Path,
) -> Result<Gathered, IngestError> {
    config.check()?;
    let full = repo.to_string();
    let mut bases = MergeBases::read(dir).map_err(IngestError::Cache)?;
    let mut open = HashMap::new();
    for pr in source.pulls(&full, Pulls::Open)? {
        let Ok(held) = held(source, &mut bases, &full, &pr) else {
            break;
        };
        open.insert(pr.number, held);
    }
    Ok(Gathered {
        config: config.clone(),
        repo: repo.clone(),
        bases,
        open,
    })
}

fn held(
    source: &dyn Source,
    bases: &mut MergeBases,
    repo: &str,
    pr: &PullRequest,
) -> Result<Held, IngestError> {
    let head = commit(&pr.head.sha)?;
    let base = commit(&bases.get(source, repo, &pr.base.sha, head.as_str())?)?;
    Ok(Held {
        revision: RevisionId::new(&base, &head),
        results: Results::of(source, repo, pr)?,
    })
}

/// Brings the ledger up to date with what's happening in the repo
/// `gathered` was read from: every open PR, since its checks and reviews
/// come and go, and every recently closed one the ledger doesn't hold as it
/// finished yet. The merge base of a PR's base and head never changes, so
/// it comes from the cache beside the ledger, and a closed PR the ledger
/// already holds as GitHub shows it costs no calls.
///
/// The PRs are listed again here, under the lock: another writer may have
/// recorded one as it was after it was gathered, and a PR is never
/// recorded as it was before what the ledger holds. A PR's gathered
/// results are its own while it's at the revision they were read at: they
/// stay bound to that merge base and head, and one pushed to, or whose base
/// moved or was rewritten so its merge base changed, is read again, as is
/// one opened since. Each PR's timeline is read here, after its results,
/// so a result from before its latest retarget is left out, however many
/// there were since the gather and wherever they went.
pub fn sync(
    store: &mut Store,
    source: &dyn Source,
    gathered: Gathered,
) -> Result<Vec<Ingested>, IngestError> {
    let Gathered {
        config,
        repo,
        bases: asked,
        mut open,
    } = gathered;
    config.check()?;
    let full = repo.to_string();
    let mut bases = MergeBases::load(store.lock()).map_err(IngestError::Cache)?;
    bases.keep(asked).map_err(IngestError::Cache)?;
    let mut got = Vec::new();
    for pr in source.pulls(&full, Pulls::Open)? {
        let base = commit(&bases.get(source, &full, &pr.base.sha, &pr.head.sha)?)?;
        let revision = RevisionId::new(&base, &commit(&pr.head.sha)?);
        let read = match open.remove(&pr.number) {
            Some(h) if h.revision == revision => Read::after(source, &full, &pr, h.results)?,
            _ => Read::of(source, &full, &pr)?,
        };
        got.push(record(store, source, &config, &repo, &pr, base, read)?);
    }
    for pr in source.pulls(&full, Pulls::RecentlyClosed)?.iter().rev() {
        let base = commit(&bases.get(source, &full, &pr.base.sha, &pr.head.sha)?)?;
        let revision = RevisionId::new(&base, &commit(&pr.head.sha)?);
        let ledger = store.ledger();
        let settled = ledger
            .by_pr(&repo, pr.number)
            .is_some_and(|c| settled(ledger, c, pr, revision));
        if !settled {
            let read = Read::of(source, &full, pr)?;
            got.push(record(store, source, &config, &repo, pr, base, read)?);
        }
    }
    Ok(got)
}

/// Whether the ledger already holds a closed PR as GitHub shows it: how it
/// finished, the revision it's at and its intent. A PR reopened, retargeted or
/// pushed to (back to an older head or base too) and closed again, or
/// reopened and merged, between two syncs is read again.
fn settled(ledger: &Ledger, c: &Change, pr: &PullRequest, revision: RevisionId) -> bool {
    let finished = match c.status() {
        Status::Open => false,
        Status::Landed | Status::Reverted => pr.is_merged(),
        // Closed without landing, or merged without a landing: into another
        // branch, or pushed to trunk outside a merge.
        Status::Closed => {
            !pr.is_merged()
                || !pr.targets_trunk()
                || pr.merge_commit_sha.as_deref().is_some_and(|sha| {
                    ledger
                        .outside()
                        .iter()
                        .any(|o| o.repo == c.repo && o.after.as_str() == sha)
                })
        }
    };
    let body = pr.body.as_deref().unwrap_or_default();
    finished
        && c.current == Some(revision)
        && c.intent
            .as_ref()
            .is_some_and(|i| i.summary == pr.title && i.body == body)
}
