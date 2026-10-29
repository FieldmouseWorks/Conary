//! A pull request's state on GitHub, as ledger ops. Ingest reads the PR as it
//! is now and records whatever the ledger doesn't hold yet, so running it on
//! every PR event builds up the change's history, and running it twice
//! records nothing the second time.
//!
//! It only ever reads GitHub. Evidence is recorded as GitHub reports it and is
//! never counted for landing here (`docs/DESIGN.md` section 4.3).

use std::sync::LazyLock;

use regex::Regex;

use conary_core::ledger::{Change, Invalid, Ledger, Status};
use conary_core::op::{Actor, ActorKind, Authority, Body, CheckKind, Outcome, Repo};
use conary_core::store::StoreError;
use conary_core::{ChangeId, CommitId, RevisionId, Store};

use crate::api::{
    ApiError, CheckRun, Commit, PullRequest, Pulls, Review, Source, Status as CommitStatus,
};
use crate::bases::{BasesError, MergeBases};

/// What ingest trusts, from the machine's configuration. Both are needed:
/// without them no merge could be told from a bypass.
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// The steward's logins, as REST spells them: its GitHub App's bot,
    /// `SLUG[bot]`, which no user's login can be. Their review markers are
    /// recorded as reviews, and only their own statuses are its word.
    /// Anyone else's marker is text in a comment.
    pub reviewers: Vec<String>,
    /// The steward's own status contexts. Its "settled" status is its landing
    /// decision, not evidence about the change, and a merge into trunk is a
    /// landing only on a head it settled.
    pub own_contexts: Vec<String>,
}

impl Config {
    /// Whether a status in `context` is a check's result about its commit:
    /// neither the steward's word nor the train's own status.
    pub(crate) fn is_check(&self, context: &str) -> bool {
        !self.own_contexts.iter().any(|c| c == context) && context != crate::train::CONTEXT
    }

    fn check(&self) -> Result<(), IngestError> {
        if self.reviewers.is_empty() {
            return Err(IngestError::Config("no reviewer logins: the steward's"));
        }
        if self.own_contexts.is_empty() {
            return Err(IngestError::Config(
                "no own status contexts: the steward's settled status",
            ));
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum IngestError {
    Api(ApiError),
    Store(StoreError),
    /// GitHub's answer is missing something or isn't what it says.
    Malformed(String),
    /// No randomness for a new change id.
    Random(String),
    /// What ingest needs to know isn't configured; nothing was read.
    Config(&'static str),
    /// The merge-base cache beside the ledger can't be read or added to.
    Cache(std::io::Error),
}

impl std::fmt::Display for IngestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Api(e) => e.fmt(f),
            Self::Store(e) => e.fmt(f),
            Self::Malformed(what) => write!(f, "GitHub sent {what}"),
            Self::Random(e) => write!(f, "no randomness for a change id: {e}"),
            Self::Config(what) => write!(f, "{what} isn't configured"),
            Self::Cache(e) => write!(f, "merge-base cache: {e}"),
        }
    }
}

impl std::error::Error for IngestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Api(e) => Some(e),
            _ => None,
        }
    }
}

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

impl From<BasesError> for IngestError {
    fn from(e: BasesError) -> Self {
        match e {
            BasesError::Api(e) => Self::Api(e),
            BasesError::Io(e) => Self::Cache(e),
        }
    }
}

/// What one PR's ingest did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ingested {
    pub change: ChangeId,
    /// The PR's head on its base as GitHub shows it now.
    pub revision: RevisionId,
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
    config.check()?;
    let pr = source.pull(&repo.to_string(), number)?;
    let base = base(source, repo, &pr)?;
    record(store, source, config, repo, &pr, base)
}

/// Where the head branched from the base branch: the tree the PR's diff and
/// its review are against, which trunk moving doesn't change.
fn base(source: &dyn Source, repo: &Repo, pr: &PullRequest) -> Result<CommitId, IngestError> {
    commit(&source.merge_base(&repo.to_string(), &pr.base.sha, &pr.head.sha)?)
}

/// Ingests every PR in the repo, oldest first.
pub fn backfill(
    store: &mut Store,
    source: &dyn Source,
    config: &Config,
    repo: &Repo,
) -> Result<Vec<Ingested>, IngestError> {
    config.check()?;
    source
        .pulls(&repo.to_string(), Pulls::All)?
        .iter()
        .map(|pr| {
            let base = base(source, repo, pr)?;
            record(store, source, config, repo, pr, base)
        })
        .collect()
}

/// Brings the ledger up to date with what's happening in a repo: every open
/// PR, since its checks and reviews come and go, and every recently closed
/// one the ledger doesn't hold as it finished yet. The merge base of a
/// PR's base and head never changes, so it comes from the cache beside the
/// ledger, and a closed PR the ledger already holds as GitHub shows it
/// costs no calls.
pub fn sync(
    store: &mut Store,
    source: &dyn Source,
    config: &Config,
    repo: &Repo,
) -> Result<Vec<Ingested>, IngestError> {
    config.check()?;
    let full = repo.to_string();
    let mut bases = MergeBases::load(store.lock()).map_err(IngestError::Cache)?;
    let mut got = Vec::new();
    for pr in source.pulls(&full, Pulls::Open)? {
        let base = commit(&bases.get(source, &full, &pr.base.sha, &pr.head.sha)?)?;
        got.push(record(store, source, config, repo, &pr, base)?);
    }
    for pr in source.pulls(&full, Pulls::RecentlyClosed)?.iter().rev() {
        let base = commit(&bases.get(source, &full, &pr.base.sha, &pr.head.sha)?)?;
        let revision = RevisionId::new(&base, &commit(&pr.head.sha)?);
        let ledger = store.ledger();
        let settled = ledger
            .by_pr(repo, pr.number)
            .is_some_and(|c| settled(ledger, c, pr, revision));
        if !settled {
            got.push(record(store, source, config, repo, pr, base)?);
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
        Status::Landed => pr.is_merged(),
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
    base: CommitId,
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

    let head = commit(&pr.head.sha)?;
    let revision = RevisionId::new(&base, &head);
    w.record(Body::Revision {
        change,
        base: base.clone(),
        head: head.clone(),
    })?;

    // Results are read by head, and a head keeps its results when the PR is
    // retargeted, so they're bound to this revision only when they came
    // after the last retarget and aren't bound to another revision of this
    // head already. One whose revision can't be told is left out.
    let since = source.retargeted(&full, pr.number)?;
    let current = |at: Option<&str>| since.as_deref().is_none_or(|s| at.is_some_and(|t| t >= s));
    let mut evidence = Vec::new();
    for run in source.check_runs(&full, head.as_str())? {
        // Started after the retarget, so it can't have run on the old base.
        if current(run.started_at.as_deref()) {
            evidence.extend(run_result(&run).map(|f| f.evidence(change, revision)));
        }
    }
    // Newest first from GitHub; recorded in the order they happened.
    let statuses = source.statuses(&full, head.as_str())?;
    for status in statuses.iter().rev() {
        if config.is_check(&status.context) && current(status.created_at.as_deref()) {
            evidence.extend(status_result(status).map(|f| f.evidence(change, revision)));
        }
    }
    // Newest first: the steward's latest word on this head since the last
    // retarget and before it merged, so a merge it hadn't settled yet (an
    // administrator's) stays a bypass whatever it says after. Anyone who can
    // push can post its context; only the steward's own logins say it.
    let before_merge = |at: Option<&str>| {
        pr.merged_at
            .as_deref()
            .is_some_and(|m| at.is_some_and(|t| t <= m))
    };
    let settled = statuses
        .iter()
        .filter(|s| config.own_contexts.contains(&s.context))
        .filter(|s| {
            s.creator
                .as_ref()
                .is_some_and(|u| config.reviewers.contains(&u.login))
        })
        .filter(|s| before_merge(s.created_at.as_deref()))
        .find(|s| current(s.created_at.as_deref()))
        .filter(|s| s.state == "success")
        .and_then(|s| s.creator.as_ref())
        .map(|u| Authority::Status {
            login: u.login.clone(),
        });
    for review in source.reviews(&full, pr.number)? {
        if current(review.submitted_at.as_deref()) {
            evidence.extend(review_marker(config, pr, change, revision, &review));
        }
    }
    for body in evidence {
        let c = w.store.ledger().change(change).expect("observed above");
        if !bound_elsewhere(c, revision, &head, &body) {
            w.record(body)?;
        }
    }

    let status = w
        .store
        .ledger()
        .change(change)
        .map(|c| c.status())
        .expect("observed above");
    if pr.is_merged() {
        merged(&mut w, source, repo, pr, change, revision, status, settled)?;
    } else {
        match (pr.state.as_str(), status) {
            ("closed", Status::Open) => w.record(Body::Close { change })?,
            ("open", Status::Closed) => w.record(Body::Reopen { change })?,
            _ => {}
        }
    }

    Ok(Ingested {
        change,
        revision,
        recorded: w.recorded,
    })
}

/// Whether a result is already bound to another revision with the same head:
/// a PR's earlier base, before it was retargeted or its base branch rewritten.
fn bound_elsewhere(c: &Change, revision: RevisionId, head: &CommitId, body: &Body) -> bool {
    let Body::Evidence {
        kind,
        check,
        result,
        source,
        ..
    } = body
    else {
        return false;
    };
    c.evidence.iter().any(|e| {
        e.revision != revision
            && c.revision(e.revision).is_some_and(|r| &r.head == head)
            && (&e.kind, &e.check, &e.result, &e.source) == (kind, check, result, source)
    })
}

/// A merged PR landed only if the steward settled its head and the trunk
/// commit is this PR's own merge: the ruleset holds every merge into trunk to
/// the steward's status, and a merge, squash or rebase always makes a commit
/// of its own. A merge commit is one when its second parent is the PR's
/// head and GitHub made and signed it: a local merge of the same head,
/// pushed, has the same shape. A squash or rebase is one when GitHub links
/// it to the PR, which it doesn't for commits made locally. (GitHub doesn't
/// sign a rebase's commits as itself, so a signature can't be asked of all
/// three.) Merged into another branch, it closed without
/// landing on trunk; its commits land with that branch. Brought onto trunk
/// by another PR's merge, it closed, and that PR records the movement. Anything
/// else that put it on trunk, an admin's bypass or a push straight to trunk,
/// moved trunk outside a landing: that's recorded as a violation, from a
/// trunk before GitHub doesn't say, and the change closes. None of it
/// depends on which PR is read first.
#[allow(clippy::too_many_arguments)]
fn merged(
    w: &mut Writer,
    source: &dyn Source,
    repo: &Repo,
    pr: &PullRequest,
    change: ChangeId,
    revision: RevisionId,
    status: Status,
    settled: Option<Authority>,
) -> Result<(), IngestError> {
    if status == Status::Landed {
        return Ok(());
    }
    let close = |w: &mut Writer| match status {
        Status::Open => w.record(Body::Close { change }),
        _ => Ok(()),
    };
    if !pr.targets_trunk() {
        return close(w);
    }
    let full = repo.to_string();
    let sha = pr
        .merge_commit_sha
        .as_deref()
        .ok_or_else(|| IngestError::Malformed("a merged PR with no merge commit".into()))?;
    let after = commit(sha)?;
    let landed = source.commit(&full, sha)?;
    let linked = source.commit_pulls(&full, sha)?;
    let own = sha != pr.head.sha
        && match landed.parents.as_slice() {
            [_, merged] => merged.sha == pr.head.sha && landed.made_by_github(),
            [_] => linked.contains(&pr.number),
            _ => false,
        };
    if !own && linked.iter().any(|&n| n != pr.number) {
        return close(w);
    }
    if let (Some(by), true) = (settled, own) {
        let before = trunk_before(source, &full, pr.number, &landed)?;
        if status == Status::Closed {
            w.record(Body::Reopen { change })?;
        }
        return w.record(Body::Land {
            change,
            revision,
            before,
            after,
            by,
        });
    }
    // This PR's merge, read again, is the same movement.
    let seen = w
        .store
        .ledger()
        .outside()
        .iter()
        .any(|o| &o.repo == repo && o.after == after);
    if !seen {
        w.record(Body::Outside {
            repo: repo.clone(),
            before: None,
            after,
        })?;
    }
    close(w)
}

/// GitHub merges at most 250 commits; a walk longer than this isn't a landing.
const MAX_LANDED: usize = 300;

/// Trunk just before the commits a landing made, `landed` on top. A merge commit's first
/// parent is. A squash or rebase lays the PR's commits on trunk in a line,
/// each linked to the PR, so trunk before is the first commit back that
/// isn't; counting the PR's own commits would miss the empty ones a rebase
/// drops.
fn trunk_before(
    source: &dyn Source,
    repo: &str,
    number: u64,
    landed: &Commit,
) -> Result<CommitId, IngestError> {
    let mut at = landed.clone();
    for _ in 0..MAX_LANDED {
        let parent = at.parents.first().ok_or_else(|| {
            IngestError::Malformed(format!("trunk commit {} with no parent", at.sha))
        })?;
        if landed.parents.len() > 1 || !source.commit_pulls(repo, &parent.sha)?.contains(&number) {
            return commit(&parent.sha);
        }
        at = source.commit(repo, &parent.sha)?;
    }
    Err(IngestError::Malformed(format!(
        "#{number} landing more than {MAX_LANDED} commits"
    )))
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

/// One CI result on a commit, as GitHub reports it: its check, its result
/// and what produced it.
pub(crate) struct Found {
    pub check: String,
    pub result: Outcome,
    pub source: String,
}

impl Found {
    fn evidence(self, change: ChangeId, revision: RevisionId) -> Body {
        Body::Evidence {
            change,
            revision,
            kind: CheckKind::Ci,
            check: self.check,
            result: self.result,
            source: self.source,
        }
    }
}

pub(crate) fn run_result(run: &CheckRun) -> Option<Found> {
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
    Some(Found {
        check: run.name.clone(),
        result,
        source: run
            .html_url
            .clone()
            .unwrap_or_else(|| format!("check run {}", run.name)),
    })
}

pub(crate) fn status_result(status: &CommitStatus) -> Option<Found> {
    let result = match status.state.as_str() {
        "success" => Outcome::Pass,
        "failure" => Outcome::Fail,
        "error" => Outcome::Error,
        _ => return None,
    };
    Some(Found {
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
