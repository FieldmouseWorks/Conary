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
}

#[derive(Debug, Clone, Deserialize)]
pub struct CheckRun {
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub html_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CheckRuns {
    check_runs: Vec<CheckRun>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Status {
    pub context: String,
    pub state: String,
    pub target_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Review {
    pub user: Option<User>,
    pub body: Option<String>,
    pub commit_id: Option<String>,
    pub html_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Parent {
    pub sha: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Commit {
    pub sha: String,
    pub parents: Vec<Parent>,
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

/// Where ingest reads GitHub from.
pub trait Source {
    fn pull(&self, repo: &str, number: u64) -> Result<PullRequest, ApiError>;
    /// Every PR in the repo, all states, oldest first.
    fn pulls(&self, repo: &str) -> Result<Vec<PullRequest>, ApiError>;
    fn check_runs(&self, repo: &str, sha: &str) -> Result<Vec<CheckRun>, ApiError>;
    fn statuses(&self, repo: &str, sha: &str) -> Result<Vec<Status>, ApiError>;
    fn reviews(&self, repo: &str, number: u64) -> Result<Vec<Review>, ApiError>;
    fn commit(&self, repo: &str, sha: &str) -> Result<Commit, ApiError>;
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
        let url = format!("{}{path}", self.base);
        self.agent
            .get(&url)
            .header("Authorization", &format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .call()
            .and_then(|mut r| r.body_mut().read_json())
            // The error names the URL, never the token.
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

    fn pulls(&self, repo: &str) -> Result<Vec<PullRequest>, ApiError> {
        self.pages(&format!(
            "/repos/{repo}/pulls?state=all&sort=created&direction=asc"
        ))
    }

    fn check_runs(&self, repo: &str, sha: &str) -> Result<Vec<CheckRun>, ApiError> {
        let runs: CheckRuns = self.get(&format!(
            "/repos/{repo}/commits/{sha}/check-runs?per_page=100"
        ))?;
        Ok(runs.check_runs)
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

    fn merge_base(&self, repo: &str, a: &str, b: &str) -> Result<String, ApiError> {
        // One page of files is all a comparison costs; only its base is read.
        let c: Comparison = self.get(&format!("/repos/{repo}/compare/{a}...{b}?per_page=1"))?;
        Ok(c.merge_base_commit.sha)
    }
}
