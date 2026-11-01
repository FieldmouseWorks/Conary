//! The few GitHub REST reads ingest needs, behind [`Source`] so tests can
//! feed it recorded answers. REST only: it's billed per call, not by the
//! shape of the query, and every automation on the box shares the quota.

use serde::Deserialize;
use serde::de::DeserializeOwned;
use ureq::tls::{RootCerts, TlsConfig};

use crate::etags::Etags;
use crate::tokens::Tokens;

#[derive(Debug, Clone, Deserialize)]
pub struct User {
    pub login: String,
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Ref {
    pub sha: String,
    #[serde(rename = "ref")]
    pub name: String,
    /// The repository the branch is in; null for a deleted fork's head.
    pub repo: Option<RefRepo>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RefRepo {
    pub default_branch: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PullRequest {
    pub number: u64,
    pub state: String,
    pub title: String,
    pub body: Option<String>,
    pub user: User,
    pub merged: Option<bool>,
    pub merged_at: Option<String>,
    pub merge_commit_sha: Option<String>,
    pub base: Ref,
    pub head: Ref,
}

impl PullRequest {
    /// The list endpoint leaves out `merged`; `merged_at` is on both.
    pub fn is_merged(&self) -> bool {
        self.merged.unwrap_or(false) || self.merged_at.is_some()
    }

    /// Whether the PR is against its repository's trunk, the default branch.
    pub fn targets_trunk(&self) -> bool {
        self.base
            .repo
            .as_ref()
            .is_some_and(|r| r.default_branch == self.base.name)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CheckRun {
    /// GitHub's id for the run. A rerun is a new run, with a higher id.
    pub id: u64,
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub html_url: Option<String>,
    pub started_at: Option<String>,
    /// The suite it ran in: one workflow run's, under GitHub Actions. A
    /// rerun is in the suite of the run it reruns.
    pub check_suite: Option<CheckSuite>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CheckSuite {
    pub id: u64,
}

#[derive(Debug, Deserialize)]
struct CheckRuns {
    total_count: usize,
    check_runs: Vec<CheckRun>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Status {
    pub context: String,
    pub state: String,
    pub description: Option<String>,
    pub target_url: Option<String>,
    pub created_at: Option<String>,
    /// Who posted it: anyone who can push can post any context.
    pub creator: Option<User>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Review {
    pub user: Option<User>,
    pub body: Option<String>,
    pub commit_id: Option<String>,
    pub html_url: Option<String>,
    pub submitted_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Parent {
    pub sha: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Commit {
    pub sha: String,
    pub parents: Vec<Parent>,
    /// The GitHub account the committer's email belongs to.
    pub committer: Option<Login>,
    pub commit: Option<CommitDetail>,
}

impl Commit {
    /// Whether GitHub itself made this commit, merging a PR from the web or
    /// the API: committed as `web-flow` and signed with GitHub's key. A merge
    /// made locally and pushed can't be.
    pub fn made_by_github(&self) -> bool {
        self.committer
            .as_ref()
            .is_some_and(|c| c.login == "web-flow")
            && self
                .commit
                .as_ref()
                .and_then(|c| c.verification.as_ref())
                .is_some_and(|v| v.verified)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Login {
    pub login: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CommitDetail {
    pub verification: Option<Verification>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Verification {
    pub verified: bool,
}

#[derive(Debug, Deserialize)]
struct PullNumber {
    number: u64,
}

#[derive(Debug, Deserialize)]
struct TimelineEvent {
    event: Option<String>,
    created_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Comparison {
    merge_base_commit: Parent,
}

#[derive(Debug)]
pub struct ApiError {
    what: String,
    /// GitHub couldn't be asked just now: its rate limit, its own errors, or
    /// the network. Asking again later is the whole fix.
    pub unavailable: bool,
}

impl ApiError {
    pub fn new(what: impl Into<String>) -> Self {
        Self {
            what: what.into(),
            unavailable: false,
        }
    }

    pub fn unavailable(what: impl Into<String>) -> Self {
        Self {
            what: what.into(),
            unavailable: true,
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GitHub: {}", self.what)
    }
}

impl std::error::Error for ApiError {}

/// Which of a repo's PRs to list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pulls {
    /// Every one, all states, oldest first.
    All,
    /// The open ones.
    Open,
    /// The hundred closed most recently updated, newest first.
    RecentlyClosed,
}

/// Where ingest reads GitHub from.
pub trait Source {
    fn pull(&self, repo: &str, number: u64) -> Result<PullRequest, ApiError>;
    fn pulls(&self, repo: &str, which: Pulls) -> Result<Vec<PullRequest>, ApiError>;
    fn check_runs(&self, repo: &str, sha: &str) -> Result<Vec<CheckRun>, ApiError>;
    fn statuses(&self, repo: &str, sha: &str) -> Result<Vec<Status>, ApiError>;
    fn reviews(&self, repo: &str, number: u64) -> Result<Vec<Review>, ApiError>;
    fn commit(&self, repo: &str, sha: &str) -> Result<Commit, ApiError>;
    /// The PRs GitHub links a commit to: the one whose merge, squash or
    /// rebase made it, for a commit on trunk.
    fn commit_pulls(&self, repo: &str, sha: &str) -> Result<Vec<u64>, ApiError>;
    /// When the PR's base branch was last changed, if it ever was.
    fn retargeted(&self, repo: &str, number: u64) -> Result<Option<String>, ApiError>;
    /// The best common ancestor of two commits.
    fn merge_base(&self, repo: &str, a: &str, b: &str) -> Result<String, ApiError>;
}

/// The REST API, with its tokens, and the writer's [`Etags`] if it keeps them.
pub struct Api {
    agent: ureq::Agent,
    base: String,
    tokens: Tokens,
    etags: Option<Etags>,
}

/// Pages are read until one comes back short, up to this many.
const MAX_PAGES: usize = 50;

impl Api {
    /// `base` is the API root, `https://api.github.com` on github.com.
    pub fn new(base: &str, tokens: Tokens) -> Self {
        Self::through(base, tokens, ureq::Proxy::try_from_env())
    }

    /// Through `proxy`, or straight to GitHub when `None`.
    fn through(base: &str, tokens: Tokens, proxy: Option<ureq::Proxy>) -> Self {
        let config = ureq::Agent::config_builder()
            .proxy(proxy)
            .tls_config(
                TlsConfig::builder()
                    .root_certs(RootCerts::PlatformVerifier)
                    .build(),
            )
            .user_agent("conary")
            // Every status is read here: a 304 is an answer, and which
            // errors are GitHub's limits is told from their headers.
            .http_status_as_error(false)
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
            base: base.trim_end_matches('/').to_owned(),
            tokens,
            etags: None,
        }
    }

    /// Reads ask GitHub whether what's kept in `etags` is still current, and
    /// an answer that it is costs none of the rate limit.
    pub fn with_etags(mut self, etags: Etags) -> Self {
        self.etags = Some(etags);
        self
    }

    /// The Authorization header for `path`: its repository's token.
    fn authorization(&self, path: &str) -> Result<String, ApiError> {
        let repo = repo_of(path)
            .ok_or_else(|| ApiError::new(format!("{}{path}: not a repository's", self.base)))?;
        Ok(format!("Bearer {}", self.tokens.of(repo)?))
    }

    fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        self.get_found(path)?
            .ok_or_else(|| ApiError::new(format!("{}{path}: not found", self.base)))
    }

    /// `None` for a 404.
    fn get_found<T: DeserializeOwned>(&self, path: &str) -> Result<Option<T>, ApiError> {
        let url = format!("{}{path}", self.base);
        let kept = self.etags.as_ref().and_then(|e| e.get(&url));
        let mut request = self
            .agent
            .get(&url)
            .header("Authorization", &self.authorization(path)?)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some((etag, _)) = &kept {
            request = request.header("If-None-Match", etag);
        }
        let mut response = request.call().map_err(|e| unreachable(&url, e))?;
        let body = match response.status().as_u16() {
            304 => match kept {
                Some((_, body)) => body,
                None => return Err(ApiError::new(format!("{url}: 304 for nothing asked"))),
            },
            404 => return Ok(None),
            200..=299 => {
                let body = read(&url, &mut response)?;
                if let (Some(etags), Some(etag)) = (&self.etags, etag(&response)) {
                    // Only a cache: one that can't be written costs calls later.
                    _ = etags.put(&url, etag, &body);
                }
                body
            }
            _ => return Err(failed(&url, &mut response)),
        };
        parse(&url, &body).map(Some)
    }

    /// A write: `POST`, or `PATCH` when `patch`, with a JSON body.
    fn send<T: DeserializeOwned>(
        &self,
        patch: bool,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<T, ApiError> {
        let url = format!("{}{path}", self.base);
        let request = if patch {
            self.agent.patch(&url)
        } else {
            self.agent.post(&url)
        };
        let mut response = request
            .header("Authorization", &self.authorization(path)?)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send_json(body)
            .map_err(|e| unreachable(&url, e))?;
        if !response.status().is_success() {
            return Err(failed(&url, &mut response));
        }
        parse(&url, &read(&url, &mut response)?)
    }

    fn pages<T: DeserializeOwned>(&self, path: &str) -> Result<Vec<T>, ApiError> {
        let sep = if path.contains('?') { '&' } else { '?' };
        let mut all = Vec::new();
        for page in 1..=MAX_PAGES {
            let got: Vec<T> = self.get(&format!("{path}{sep}per_page=100&page={page}"))?;
            let short = got.len() < 100;
            all.extend(got);
            if short {
                return Ok(all);
            }
        }
        Err(ApiError::new(format!(
            "{path}: more than {MAX_PAGES} pages"
        )))
    }
}

/// The repository (OWNER/NAME) a path is under, `/repos/OWNER/NAME/...`:
/// every path asked here is.
fn repo_of(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/repos/")?;
    let owner = rest.find('/')?;
    let end = rest[owner + 1..]
        .find(['/', '?'])
        .map_or(rest.len(), |i| owner + 1 + i);
    (owner > 0 && end > owner + 1).then(|| &rest[..end])
}

type Response = ureq::http::Response<ureq::Body>;

/// A request that got no answer. Only what asking again can fix is GitHub
/// unavailable: the network, DNS, a timeout, a connection dropped or stalled,
/// a proxy that didn't answer or is itself unavailable. A bad URL or header, a
/// proxy that refuses (its credentials, its policy), a TLS setup or an answer
/// past the size limit stays as wrong the next time. The error names the URL,
/// never the token.
fn unreachable(url: &str, e: ureq::Error) -> ApiError {
    let passing = match &e {
        ureq::Error::Io(_)
        | ureq::Error::Timeout(_)
        | ureq::Error::HostNotFound
        | ureq::Error::ConnectionFailed
        | ureq::Error::BodyStalled => true,
        ureq::Error::ConnectProxyFailed(reason) => proxy_status(reason).is_none_or(passing_status),
        _ => false,
    };
    let what = format!("{url}: {e}");
    if passing {
        ApiError::unavailable(what)
    } else {
        ApiError::new(what)
    }
}

/// The status a proxy refused a tunnel with, `None` when it gave none.
/// ureq carries it only in its reason, "proxy server responded 407/...".
fn proxy_status(reason: &str) -> Option<u16> {
    reason
        .strip_prefix("proxy server responded ")?
        .split('/')
        .next()?
        .parse()
        .ok()
}

/// Whether an error status, GitHub's or a proxy's, is a server too busy or in
/// trouble to answer just now, rather than the request refused.
fn passing_status(status: u16) -> bool {
    status == 429 || status >= 500
}

fn read(url: &str, response: &mut Response) -> Result<Vec<u8>, ApiError> {
    response
        .body_mut()
        .read_to_vec()
        .map_err(|e| unreachable(url, e))
}

fn parse<T: DeserializeOwned>(url: &str, body: &[u8]) -> Result<T, ApiError> {
    serde_json::from_slice(body).map_err(|e| ApiError::new(format!("{url}: {e}")))
}

fn header<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
    response.headers().get(name).and_then(|v| v.to_str().ok())
}

fn etag(response: &Response) -> Option<&str> {
    header(response, "etag")
}

/// An error status. GitHub's limits, a 429 and its own 5xx are passing:
/// GitHub is unavailable, not the request wrong. A limit is a 403 or 429
/// whose headers say so (the primary one spent, a secondary one's
/// Retry-After) or whose message does: a secondary limit can come with
/// neither header, and a message lost to the network leaves it unknown, so
/// unavailable too. Anything else refused, a permission or validation
/// failure, stays refused.
fn failed(url: &str, response: &mut Response) -> ApiError {
    let status = response.status().as_u16();
    let what = format!("{url}: http status: {status}");
    let limited = header(response, "x-ratelimit-remaining") == Some("0")
        || header(response, "retry-after").is_some();
    if passing_status(status) || status == 403 && limited {
        return ApiError::unavailable(what);
    }
    if status != 403 {
        return ApiError::new(what);
    }
    match says_rate_limit(url, response) {
        Ok(true) => ApiError::unavailable(what),
        Ok(false) => ApiError::new(what),
        Err(e) if e.unavailable => {
            ApiError::unavailable(format!("{what}, its message unread: {e}"))
        }
        Err(e) => ApiError::new(format!("{what}, its message unread: {e}")),
    }
}

/// Whether an error answer's message is GitHub's about a rate limit
/// ("API rate limit exceeded", "You have exceeded a secondary rate limit").
/// One that isn't JSON isn't GitHub's; one that can't be read is an error.
fn says_rate_limit(url: &str, response: &mut Response) -> Result<bool, ApiError> {
    #[derive(Deserialize)]
    struct Message {
        message: String,
    }
    let body = response
        .body_mut()
        .with_config()
        .limit(MAX_MESSAGE)
        .read_to_vec()
        .map_err(|e| unreachable(url, e))?;
    Ok(serde_json::from_slice::<Message>(&body)
        .is_ok_and(|m| m.message.to_lowercase().contains("rate limit")))
}

/// The most of an error answer read for its message.
const MAX_MESSAGE: u64 = 64 * 1024;

impl Source for Api {
    fn pull(&self, repo: &str, number: u64) -> Result<PullRequest, ApiError> {
        self.get(&format!("/repos/{repo}/pulls/{number}"))
    }

    fn pulls(&self, repo: &str, which: Pulls) -> Result<Vec<PullRequest>, ApiError> {
        match which {
            Pulls::All => self.pages(&format!(
                "/repos/{repo}/pulls?state=all&sort=created&direction=asc"
            )),
            Pulls::Open => self.pages(&format!("/repos/{repo}/pulls?state=open")),
            Pulls::RecentlyClosed => self.get(&format!(
                "/repos/{repo}/pulls?state=closed&sort=updated&direction=desc&per_page=100"
            )),
        }
    }

    fn check_runs(&self, repo: &str, sha: &str) -> Result<Vec<CheckRun>, ApiError> {
        // Every run, reruns included, not only each check's latest; the runs
        // come wrapped with their count rather than as a bare page.
        let path = format!("/repos/{repo}/commits/{sha}/check-runs?filter=all");
        let mut all = Vec::new();
        for page in 1..=MAX_PAGES {
            let got: CheckRuns = self.get(&format!("{path}&per_page=100&page={page}"))?;
            let short = got.check_runs.len() < 100;
            all.extend(got.check_runs);
            if short || all.len() >= got.total_count {
                return Ok(all);
            }
        }
        Err(ApiError::new(format!(
            "{path}: more than {MAX_PAGES} pages"
        )))
    }

    fn statuses(&self, repo: &str, sha: &str) -> Result<Vec<Status>, ApiError> {
        self.pages(&format!("/repos/{repo}/commits/{sha}/statuses"))
    }

    fn reviews(&self, repo: &str, number: u64) -> Result<Vec<Review>, ApiError> {
        self.pages(&format!("/repos/{repo}/pulls/{number}/reviews"))
    }

    fn commit(&self, repo: &str, sha: &str) -> Result<Commit, ApiError> {
        self.get(&format!("/repos/{repo}/commits/{sha}"))
    }

    fn commit_pulls(&self, repo: &str, sha: &str) -> Result<Vec<u64>, ApiError> {
        let pulls: Vec<PullNumber> = self.pages(&format!("/repos/{repo}/commits/{sha}/pulls"))?;
        Ok(pulls.into_iter().map(|p| p.number).collect())
    }

    fn retargeted(&self, repo: &str, number: u64) -> Result<Option<String>, ApiError> {
        let events: Vec<TimelineEvent> =
            self.pages(&format!("/repos/{repo}/issues/{number}/timeline"))?;
        Ok(events
            .into_iter()
            .filter(|e| e.event.as_deref() == Some("base_ref_changed"))
            .filter_map(|e| e.created_at)
            .max())
    }

    fn merge_base(&self, repo: &str, a: &str, b: &str) -> Result<String, ApiError> {
        // One page of files is all a comparison costs; only its base is read.
        let c: Comparison = self.get(&format!("/repos/{repo}/compare/{a}...{b}?per_page=1"))?;
        Ok(c.merge_base_commit.sha)
    }
}

#[derive(Debug, Deserialize)]
struct GitRef {
    object: GitObject,
}

#[derive(Debug, Deserialize)]
struct GitObject {
    sha: String,
}

#[derive(Debug, Deserialize)]
struct GitCommit {
    sha: String,
    message: String,
    parents: Vec<GitObject>,
}

impl crate::anchor::Anchors for Api {
    fn get(&self, repo: &str, name: &str) -> Result<Option<crate::anchor::Kept>, ApiError> {
        let short = name.strip_prefix("refs/").unwrap_or(name);
        let Some(r) = self.get_found::<GitRef>(&format!("/repos/{repo}/git/ref/{short}"))? else {
            return Ok(None);
        };
        crate::anchor::Anchors::commit(self, repo, &r.object.sha).map(Some)
    }

    fn commit(&self, repo: &str, sha: &str) -> Result<crate::anchor::Kept, ApiError> {
        let c: GitCommit = self.get(&format!("/repos/{repo}/git/commits/{sha}"))?;
        Ok(crate::anchor::Kept {
            commit: c.sha,
            message: c.message,
            parents: c.parents.into_iter().map(|p| p.sha).collect(),
        })
    }

    fn set(
        &self,
        repo: &str,
        name: &str,
        parent: Option<&str>,
        message: &str,
    ) -> Result<String, ApiError> {
        let tree: GitObject = self.send(
            false,
            &format!("/repos/{repo}/git/trees"),
            &serde_json::json!({"tree": [
                {"path": "anchor", "mode": "100644", "type": "blob", "content": message}
            ]}),
        )?;
        let commit: GitObject = self.send(
            false,
            &format!("/repos/{repo}/git/commits"),
            &serde_json::json!({"message": message, "tree": tree.sha, "parents": parent.into_iter().collect::<Vec<_>>()}),
        )?;
        self.move_ref(repo, name, parent.is_some(), &commit.sha)?;
        Ok(commit.sha)
    }
}

impl Api {
    /// Moves `name` to `commit`, a commit on the one it's at (`existing`),
    /// or makes it. A fast-forward, never a force, so a writer racing
    /// another, or a ref moved by hand, fails here.
    fn move_ref(
        &self,
        repo: &str,
        name: &str,
        existing: bool,
        commit: &str,
    ) -> Result<(), ApiError> {
        let _: serde_json::Value = if existing {
            self.send(
                true,
                &format!(
                    "/repos/{repo}/git/refs/{}",
                    name.strip_prefix("refs/").unwrap_or(name)
                ),
                &serde_json::json!({"sha": commit, "force": false}),
            )?
        } else {
            self.send(
                false,
                &format!("/repos/{repo}/git/refs"),
                &serde_json::json!({"ref": name, "sha": commit}),
            )?
        };
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct TreeCommit {
    tree: GitObject,
}

#[derive(Debug, Deserialize)]
struct NamedRef {
    #[serde(rename = "ref")]
    name: String,
}

/// The repo's default branch, as its own record says.
#[derive(Debug, Deserialize)]
struct Defaults {
    default_branch: String,
}

/// How a repo lets PRs be merged.
#[derive(Debug, Deserialize)]
struct MergeMethods {
    allow_squash_merge: Option<bool>,
    allow_merge_commit: Option<bool>,
    allow_rebase_merge: Option<bool>,
}

impl MergeMethods {
    /// The method the steward merges with too: squash, else a merge
    /// commit, else rebase. None when GitHub didn't say, for its default.
    fn first(&self) -> Option<&'static str> {
        [
            (self.allow_squash_merge, "squash"),
            (self.allow_merge_commit, "merge"),
            (self.allow_rebase_merge, "rebase"),
        ]
        .into_iter()
        .find_map(|(allowed, method)| (allowed == Some(true)).then_some(method))
    }
}

#[derive(Debug, Deserialize)]
struct Refusal {
    message: Option<String>,
}

/// What the merges API answers a merge with: the commit, and in it its tree.
#[derive(Debug, Deserialize)]
struct MergeCommit {
    sha: String,
    commit: TreeCommit,
}

impl crate::train::Forge for Api {
    fn default_branch(&self, repo: &str) -> Result<String, ApiError> {
        let r: Defaults = self.get(&format!("/repos/{repo}"))?;
        Ok(r.default_branch)
    }

    fn branch(&self, repo: &str, name: &str) -> Result<Option<String>, ApiError> {
        Ok(self
            .get_found::<GitRef>(&format!("/repos/{repo}/git/ref/heads/{name}"))?
            .map(|r| r.object.sha))
    }

    fn branches(&self, repo: &str, prefix: &str) -> Result<Vec<String>, ApiError> {
        // One answer, every match: this endpoint doesn't page.
        let refs: Vec<NamedRef> =
            self.get(&format!("/repos/{repo}/git/matching-refs/heads/{prefix}"))?;
        Ok(refs
            .into_iter()
            .filter_map(|r| r.name.strip_prefix("refs/heads/").map(str::to_owned))
            .collect())
    }

    fn create_branch(&self, repo: &str, name: &str, sha: &str) -> Result<(), ApiError> {
        let _: serde_json::Value = self.send(
            false,
            &format!("/repos/{repo}/git/refs"),
            &serde_json::json!({"ref": format!("refs/heads/{name}"), "sha": sha}),
        )?;
        Ok(())
    }

    fn delete_branch(&self, repo: &str, name: &str) -> Result<(), ApiError> {
        let path = format!("/repos/{repo}/git/refs/heads/{name}");
        let url = format!("{}{path}", self.base);
        let mut response = self
            .agent
            .delete(&url)
            .header("Authorization", &self.authorization(&path)?)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .call()
            .map_err(|e| unreachable(&url, e))?;
        match response.status().as_u16() {
            // 422 is GitHub's answer for a ref that isn't there.
            200..=299 | 404 | 422 => Ok(()),
            _ => Err(failed(&url, &mut response)),
        }
    }

    fn merge(
        &self,
        repo: &str,
        into: &str,
        head: &str,
        message: &str,
    ) -> Result<crate::train::Merged, ApiError> {
        use crate::train::Merged;
        let path = format!("/repos/{repo}/merges");
        let url = format!("{}{path}", self.base);
        let mut response = self
            .agent
            .post(&url)
            .header("Authorization", &self.authorization(&path)?)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send_json(serde_json::json!({"base": into, "head": head, "commit_message": message}))
            .map_err(|e| unreachable(&url, e))?;
        match response.status().as_u16() {
            201 => {
                let made: MergeCommit = parse(&url, &read(&url, &mut response)?)?;
                Ok(Merged::Commit {
                    sha: made.sha,
                    tree: made.commit.tree.sha,
                })
            }
            204 => Ok(Merged::Already),
            409 => Ok(Merged::Conflict),
            _ => Err(failed(&url, &mut response)),
        }
    }

    fn tree(&self, repo: &str, sha: &str) -> Result<String, ApiError> {
        let commit: TreeCommit = self.get(&format!("/repos/{repo}/git/commits/{sha}"))?;
        Ok(commit.tree.sha)
    }

    fn mark(
        &self,
        repo: &str,
        sha: &str,
        mark: crate::train::Mark,
        description: &str,
        url: Option<&str>,
    ) -> Result<(), ApiError> {
        use crate::train::{CONTEXT, Mark};
        let state = match mark {
            Mark::Landing => "success",
            Mark::Waiting => "pending",
            Mark::Red => "failure",
            Mark::Refused => "error",
        };
        // GitHub takes a description of up to 140 characters.
        let description: String = description.chars().take(140).collect();
        let mut body =
            serde_json::json!({"state": state, "context": CONTEXT, "description": description});
        if let Some(url) = url {
            body["target_url"] = serde_json::json!(url);
        }
        let _: serde_json::Value =
            self.send(false, &format!("/repos/{repo}/statuses/{sha}"), &body)?;
        Ok(())
    }

    fn land(&self, repo: &str, pr: u64, head: &str) -> Result<crate::train::PrMerge, ApiError> {
        use crate::train::PrMerge;
        let methods: MergeMethods = self.get(&format!("/repos/{repo}"))?;
        // The head that was checked, or nothing: a push since makes the sha
        // stale, and GitHub refuses it.
        let mut body = serde_json::json!({"sha": head});
        if let Some(method) = methods.first() {
            body["merge_method"] = serde_json::json!(method);
        }
        let path = format!("/repos/{repo}/pulls/{pr}/merge");
        let url = format!("{}{path}", self.base);
        let mut response = self
            .agent
            .put(&url)
            .header("Authorization", &self.authorization(&path)?)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send_json(body)
            .map_err(|e| unreachable(&url, e))?;
        match response.status().as_u16() {
            200 => Ok(PrMerge::Merged),
            // Not mergeable, a rule it doesn't meet, or the head moved.
            status @ (405 | 409 | 422) => {
                let said: Refusal =
                    parse(&url, &read(&url, &mut response)?).unwrap_or(Refusal { message: None });
                Ok(PrMerge::Refused(
                    said.message
                        .unwrap_or_else(|| format!("http status {status}")),
                ))
            }
            _ => Err(failed(&url, &mut response)),
        }
    }
}

#[derive(Debug, Deserialize)]
struct Tree {
    tree: Vec<TreeEntry>,
    truncated: bool,
}

#[derive(Debug, Deserialize)]
struct TreeEntry {
    path: String,
    #[serde(rename = "type")]
    kind: String,
    sha: String,
}

impl crate::notes::Notes for Api {
    fn get(&self, repo: &str, name: &str) -> Result<Option<crate::notes::Kept>, ApiError> {
        let short = name.strip_prefix("refs/").unwrap_or(name);
        let Some(r) = self.get_found::<GitRef>(&format!("/repos/{repo}/git/ref/{short}"))? else {
            return Ok(None);
        };
        let commit: TreeCommit =
            self.get(&format!("/repos/{repo}/git/commits/{}", r.object.sha))?;
        let tree: Tree = self.get(&format!(
            "/repos/{repo}/git/trees/{}?recursive=1",
            commit.tree.sha
        ))?;
        if tree.truncated {
            return Err(ApiError::new(format!(
                "{repo}'s {name} holds more notes than one read of its tree returns"
            )));
        }
        let notes = tree
            .tree
            .into_iter()
            .filter(|e| e.kind == "blob")
            .filter_map(|e| {
                // A note's path is the commit it's on, maybe fanned out.
                let on: String = e.path.chars().filter(|&c| c != '/').collect();
                let commit =
                    matches!(on.len(), 40 | 64) && on.bytes().all(|b| b.is_ascii_hexdigit());
                commit.then_some((on, (e.path, e.sha)))
            })
            .collect();
        Ok(Some(crate::notes::Kept {
            commit: r.object.sha,
            tree: commit.tree.sha,
            notes,
        }))
    }

    fn set(
        &self,
        repo: &str,
        name: &str,
        parent: Option<&crate::notes::Kept>,
        files: &[(String, String)],
    ) -> Result<(), ApiError> {
        let entries: Vec<_> = files
            .iter()
            .map(|(path, text)| {
                serde_json::json!({"path": path, "mode": "100644", "type": "blob", "content": text})
            })
            .collect();
        let mut tree = serde_json::json!({"tree": entries});
        if let Some(p) = parent {
            tree["base_tree"] = serde_json::json!(p.tree);
        }
        let tree: GitObject = self.send(false, &format!("/repos/{repo}/git/trees"), &tree)?;
        let message = match files.len() {
            1 => "conary: one landing's note".to_owned(),
            n => format!("conary: {n} landings' notes"),
        };
        let commit: GitObject = self.send(
            false,
            &format!("/repos/{repo}/git/commits"),
            &serde_json::json!({
                "message": message,
                "tree": tree.sha,
                "parents": parent.map(|p| &p.commit).into_iter().collect::<Vec<_>>(),
            }),
        )?;
        self.move_ref(repo, name, parent.is_some(), &commit.sha)
    }
}

mod actions;

#[cfg(test)]
mod tests;
