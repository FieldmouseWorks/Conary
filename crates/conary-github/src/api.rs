//! The few GitHub REST reads ingest needs, behind [`Source`] so tests can
//! feed it recorded answers. REST only: it's billed per call, not by the
//! shape of the query, and every automation on the box shares the quota.

use serde::Deserialize;
use serde::de::DeserializeOwned;
use ureq::tls::{RootCerts, TlsConfig};

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
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub html_url: Option<String>,
    pub started_at: Option<String>,
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
pub struct ApiError(pub String);

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GitHub: {}", self.0)
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

/// The REST API, with a token.
pub struct Api {
    agent: ureq::Agent,
    base: String,
    token: String,
}

/// Pages are read until one comes back short, up to this many.
const MAX_PAGES: usize = 50;

impl Api {
    /// `base` is the API root, `https://api.github.com` on github.com.
    pub fn new(base: &str, token: String) -> Self {
        let config = ureq::Agent::config_builder()
            .tls_config(
                TlsConfig::builder()
                    .root_certs(RootCerts::PlatformVerifier)
                    .build(),
            )
            .user_agent("conary")
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
            base: base.trim_end_matches('/').to_owned(),
            token,
        }
    }

    fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        self.get_found(path)?
            .ok_or_else(|| ApiError(format!("{}{path}: not found", self.base)))
    }

    /// `None` for a 404.
    fn get_found<T: DeserializeOwned>(&self, path: &str) -> Result<Option<T>, ApiError> {
        let url = format!("{}{path}", self.base);
        let got = self
            .agent
            .get(&url)
            .header("Authorization", &format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .call()
            .and_then(|mut r| r.body_mut().read_json());
        match got {
            Ok(v) => Ok(Some(v)),
            Err(ureq::Error::StatusCode(404)) => Ok(None),
            // The error names the URL, never the token.
            Err(e) => Err(ApiError(format!("{url}: {e}"))),
        }
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
        request
            .header("Authorization", &format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send_json(body)
            .and_then(|mut r| r.body_mut().read_json())
            .map_err(|e| ApiError(format!("{url}: {e}")))
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
        Err(ApiError(format!("{path}: more than {MAX_PAGES} pages")))
    }
}

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
        Err(ApiError(format!("{path}: more than {MAX_PAGES} pages")))
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
        // Moved only from the parent read: a fast-forward, never a force,
        // so a writer racing another, or an anchor moved by hand, fails here.
        let _: serde_json::Value = match parent {
            Some(_) => self.send(
                true,
                &format!(
                    "/repos/{repo}/git/refs/{}",
                    name.strip_prefix("refs/").unwrap_or(name)
                ),
                &serde_json::json!({"sha": commit.sha, "force": false}),
            )?,
            None => self.send(
                false,
                &format!("/repos/{repo}/git/refs"),
                &serde_json::json!({"ref": name, "sha": commit.sha}),
            )?,
        };
        Ok(commit.sha)
    }
}
