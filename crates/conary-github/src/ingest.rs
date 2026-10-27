//! A pull request's state on GitHub, as ledger ops. Ingest reads the PR as it
//! is now and records whatever the ledger doesn't hold yet, so running it on
//! every PR event builds up the change's history, and running it twice
//! records nothing the second time.
//!
//! It only ever reads GitHub. Evidence is recorded as GitHub reports it and is
//! never counted for landing here (`docs/DESIGN.md` section 4.3).

use std::sync::LazyLock;

use regex::Regex;

use conary_core::ledger::{Invalid, Status};
use conary_core::op::{Actor, ActorKind, Body, CheckKind, Outcome, Repo};
use conary_core::store::StoreError;
use conary_core::{ChangeId, CommitId, RevisionId, Store};

use crate::api::{ApiError, CheckRun, PullRequest, Pulls, Review, Source, Status as CommitStatus};

/// What ingest trusts, from the machine's configuration.
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// Logins whose review markers are recorded as reviews: the steward's
    /// account. Anyone else's marker is text in a comment.
    pub reviewers: Vec<String>,
    /// Status contexts that aren't checks: the steward's own "settled" status
    /// is its landing decision, not evidence about the change.
    pub own_contexts: Vec<String>,
}

#[derive(Debug)]
pub enum IngestError {
    Api(ApiError),
    Store(StoreError),
    /// GitHub's answer is missing something or isn't what it says.
    Malformed(String),
    /// No randomness for a new change id.
    Random(String),
}

impl std::fmt::Display for IngestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Api(e) => e.fmt(f),
            Self::Store(e) => e.fmt(f),
            Self::Malformed(what) => write!(f, "GitHub sent {what}"),
            Self::Random(e) => write!(f, "no randomness for a change id: {e}"),
        }
    }
}

impl std::error::Error for IngestError {}

impl From<ApiError> for IngestError {
    fn from(e: ApiError) -> Self {
        Self::Api(e)
    }
}

impl From<StoreError> for IngestError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

/// What one PR's ingest did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ingested {
    pub change: ChangeId,
    /// Ops written; 0 when the ledger already held everything.
    pub recorded: usize,
}

/// Ingests one PR, read fresh from GitHub.
pub fn pull(
    store: &mut Store,
    source: &dyn Source,
    config: &Config,
    repo: &Repo,
    number: u64,
) -> Result<Ingested, IngestError> {
    let pr = source.pull(&repo.to_string(), number)?;
    record(store, source, config, repo, &pr)
}

/// Ingests every PR in the repo, oldest first.
pub fn backfill(
    store: &mut Store,
    source: &dyn Source,
    config: &Config,
    repo: &Repo,
) -> Result<Vec<Ingested>, IngestError> {
    source
        .pulls(&repo.to_string(), Pulls::All)?
        .iter()
        .map(|pr| record(store, source, config, repo, pr))
        .collect()
}

/// Brings the ledger up to date with what's happening in a repo: every open
/// PR, since its checks and reviews come and go, and every recently closed
/// one the ledger doesn't hold as landed or closed yet. A PR the ledger
/// already holds as finished costs nothing.
pub fn sync(
    store: &mut Store,
    source: &dyn Source,
    config: &Config,
    repo: &Repo,
) -> Result<Vec<Ingested>, IngestError> {
    let full = repo.to_string();
    let mut got = Vec::new();
    for pr in source.pulls(&full, Pulls::Open)? {
        got.push(record(store, source, config, repo, &pr)?);
    }
    for pr in source.pulls(&full, Pulls::RecentlyClosed)?.iter().rev() {
        let finished = store
            .ledger()
            .by_pr(repo, pr.number)
            .is_some_and(|c| c.status() != Status::Open);
        if !finished {
            got.push(record(store, source, config, repo, pr)?);
        }
    }
    Ok(got)
}

fn commit(sha: &str) -> Result<CommitId, IngestError> {
    sha.parse()
        .map_err(|_| IngestError::Malformed(format!("{sha:?} as a commit id")))
}

/// Records what the ledger doesn't already hold.
struct Writer<'a> {
    store: &'a mut Store,
    recorded: usize,
}

impl Writer<'_> {
    fn record(&mut self, body: Body) -> Result<(), IngestError> {
        match self.store.record(body) {
            Ok(_) => {
                self.recorded += 1;
                Ok(())
            }
            Err(StoreError::Invalid(Invalid::Duplicate)) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

fn record(
    store: &mut Store,
    source: &dyn Source,
    config: &Config,
    repo: &Repo,
    pr: &PullRequest,
) -> Result<Ingested, IngestError> {
    let full = repo.to_string();
    let mut w = Writer { store, recorded: 0 };
    let body = pr.body.clone().unwrap_or_default();

    let change = match w.store.ledger().by_pr(repo, pr.number) {
        Some(c) => c.id,
        None => {
            let change = ChangeId::new().map_err(|e| IngestError::Random(e.to_string()))?;
            w.record(Body::Observe {
                change,
                repo: repo.clone(),
                pr: pr.number,
                author: actor(pr, &body),
            })?;
            change
        }
    };
    w.record(Body::Intent {
        change,
        summary: pr.title.clone(),
        body: body.clone(),
        task: task(repo, &body),
    })?;

    // The base is where the head branched from the base branch: the tree the
    // PR's diff and its review are against, which trunk moving doesn't change.
    let head = commit(&pr.head.sha)?;
    let base = commit(&source.merge_base(&full, &pr.base.sha, &pr.head.sha)?)?;
    let revision = RevisionId::new(&base, &head);
    w.record(Body::Revision {
        change,
        base: base.clone(),
        head: head.clone(),
    })?;

    for run in source.check_runs(&full, head.as_str())? {
        if let Some(body) = check_run(change, revision, &run) {
            w.record(body)?;
        }
    }
    // Newest first from GitHub; recorded in the order they happened.
    for status in source.statuses(&full, head.as_str())?.iter().rev() {
        if !config.own_contexts.contains(&status.context)
            && let Some(body) = commit_status(change, revision, status)
        {
            w.record(body)?;
        }
    }
    for review in source.reviews(&full, pr.number)? {
        if let Some(body) = review_marker(config, pr, change, revision, &review) {
            w.record(body)?;
        }
    }

    let status = w
        .store
        .ledger()
        .change(change)
        .map(|c| c.status())
        .expect("observed above");
    match (pr.is_merged(), pr.state.as_str(), status) {
        (true, _, Status::Open | Status::Closed) => {
            if status == Status::Closed {
                w.record(Body::Reopen { change })?;
            }
            let sha = pr
                .merge_commit_sha
                .as_deref()
                .ok_or_else(|| IngestError::Malformed("a merged PR with no merge commit".into()))?;
            let after = commit(sha)?;
            let landed = source.commit(&full, sha)?;
            let before = landed.parents.first().ok_or_else(|| {
                IngestError::Malformed(format!("merge commit {sha} with no parent"))
            })?;
            w.record(Body::Land {
                change,
                revision,
                before: commit(&before.sha)?,
                after,
            })?;
        }
        (false, "closed", Status::Open) => w.record(Body::Close { change })?,
        (false, "open", Status::Closed) => w.record(Body::Reopen { change })?,
        _ => {}
    }

    Ok(Ingested {
        change,
        recorded: w.recorded,
    })
}

static SESSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"https://claude\.ai/code/session_\w+").unwrap());

/// An agent says which session it is by linking it in the PR body.
fn actor(pr: &PullRequest, body: &str) -> Actor {
    let session = SESSION.find(body).map(|m| m.as_str().to_owned());
    let kind = if pr.user.kind == "Bot" {
        ActorKind::Bot
    } else if session.is_some() {
        ActorKind::Agent
    } else {
        ActorKind::Person
    };
    Actor {
        kind,
        login: pr.user.login.clone(),
        session,
    }
}

static TASK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?|refs?)\s+([\w.-]+/[\w.-]+)?#(\d+)\b",
    )
    .unwrap()
});

static THREAD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"https://claude\.ai/code/project/\w+\?thread=\w+").unwrap());

/// What the work came from: the first issue the body closes or refers to,
/// as `owner/name#N`, or else the project thread it links.
fn task(repo: &Repo, body: &str) -> Option<String> {
    if let Some(m) = TASK.captures(body) {
        let on = m
            .get(1)
            .map_or_else(|| repo.to_string(), |r| r.as_str().to_owned());
        return Some(format!("{on}#{}", &m[2]));
    }
    THREAD.find(body).map(|m| m.as_str().to_owned())
}

fn check_run(change: ChangeId, revision: RevisionId, run: &CheckRun) -> Option<Body> {
    if run.status != "completed" {
        return None;
    }
    let result = match run.conclusion.as_deref()? {
        "success" => Outcome::Pass,
        "failure" => Outcome::Fail,
        "timed_out" | "cancelled" | "startup_failure" | "stale" => Outcome::Error,
        // Neutral, skipped, action_required: no result about the change.
        _ => return None,
    };
    Some(Body::Evidence {
        change,
        revision,
        kind: CheckKind::Ci,
        check: run.name.clone(),
        result,
        source: run
            .html_url
            .clone()
            .unwrap_or_else(|| format!("check run {}", run.name)),
    })
}

fn commit_status(change: ChangeId, revision: RevisionId, status: &CommitStatus) -> Option<Body> {
    let result = match status.state.as_str() {
        "success" => Outcome::Pass,
        "failure" => Outcome::Fail,
        "error" => Outcome::Error,
        _ => return None,
    };
    Some(Body::Evidence {
        change,
        revision,
        kind: CheckKind::Ci,
        check: status.context.clone(),
        result,
        source: status
            .target_url
            .clone()
            .unwrap_or_else(|| format!("status {}", status.context)),
    })
}

static MARKER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<!-- (\w+)-review:v1 (\{.*?\}) -->").unwrap());

#[derive(serde::Deserialize)]
struct Marker {
    commit_sha: String,
    base: String,
    findings: u64,
}

/// A reviewer's verdict, from the marker the steward puts on each review it
/// posts: `<!-- codex-review:v1 {"commit_sha":…,"base":…,"findings":N} -->`.
/// Only a marker on this head, against this base branch, counts as being
/// about this revision: a retargeted PR is another change on the same head.
fn review_marker(
    config: &Config,
    pr: &PullRequest,
    change: ChangeId,
    revision: RevisionId,
    review: &Review,
) -> Option<Body> {
    let login = &review.user.as_ref()?.login;
    if !config.reviewers.contains(login) {
        return None;
    }
    let body = review
        .body
        .as_deref()?
        .replace("&#123;", "{")
        .replace("&#125;", "}");
    let m = MARKER.captures(&body)?;
    let said: Marker = serde_json::from_str(&m[2]).ok()?;
    if said.commit_sha != pr.head.sha
        || said.base != pr.base.name
        || review.commit_id.as_deref() != Some(pr.head.sha.as_str())
    {
        return None;
    }
    Some(Body::Evidence {
        change,
        revision,
        kind: CheckKind::Review,
        check: m[1].to_owned(),
        result: if said.findings == 0 {
            Outcome::Pass
        } else {
            Outcome::Fail
        },
        source: review
            .html_url
            .clone()
            .unwrap_or_else(|| format!("{} review by {login}", &m[1])),
    })
}

#[cfg(test)]
mod tests;
