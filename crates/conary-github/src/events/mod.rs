//! Authenticated event ingress and durable, coalesced flow work.
mod queue;
mod runtime;
use crate::flow::persist::{self, Result};
pub use queue::{Queue, Work};
use ring::hmac;
pub use runtime::Runtime;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub const MAX_BODY: usize = 1 << 20;
const DELIVERY_LIFETIME: u64 = 86400;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kick {
    pub repo: String,
    pub pr: Option<u64>,
}

#[derive(Clone, Copy)]
enum Target {
    Pull,
    Issue,
    Check(&'static str),
    Repository,
    DefaultPush,
}
// The sole event-to-kick table. PR label actions arrive as pull_request events;
// GitHub's label event describes label definitions, not attachment to a PR.
const EVENTS: &[(&str, Target)] = &[
    ("pull_request", Target::Pull),
    ("pull_request_review", Target::Pull),
    ("pull_request_review_comment", Target::Pull),
    ("pull_request_review_thread", Target::Pull),
    ("issue_comment", Target::Issue),
    ("check_suite", Target::Check("check_suite")),
    ("check_run", Target::Check("check_run")),
    ("status", Target::Repository),
    ("push", Target::DefaultPush),
];

pub fn kicks(event: &str, body: &Value, repos: &BTreeSet<String>) -> Vec<Kick> {
    let Some(repo) = body["repository"]["full_name"].as_str() else {
        return vec![];
    };
    let repo = repo.to_lowercase();
    if !repos.contains(&repo) {
        return vec![];
    }
    let Some((_, target)) = EVENTS.iter().find(|(name, _)| *name == event) else {
        return vec![];
    };
    let numbers: Vec<Option<u64>> = match target {
        Target::Pull => body["pull_request"]["number"]
            .as_u64()
            .map(Some)
            .into_iter()
            .collect(),
        Target::Issue if body["issue"]["pull_request"].is_object() => body["issue"]["number"]
            .as_u64()
            .map(Some)
            .into_iter()
            .collect(),
        Target::Issue => vec![],
        Target::Check(field) => {
            let prs: Vec<_> = body[*field]["pull_requests"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| p["number"].as_u64().map(Some))
                .collect();
            // Fork checks can have an empty PR list. Read the repo to correlate heads.
            if prs.is_empty() { vec![None] } else { prs }
        }
        Target::Repository => vec![None],
        Target::DefaultPush => {
            let default = body["repository"]["default_branch"].as_str();
            if default
                .is_some_and(|b| body["ref"].as_str() == Some(format!("refs/heads/{b}").as_str()))
            {
                vec![None]
            } else {
                vec![]
            }
        }
    };
    numbers
        .into_iter()
        .map(|pr| Kick {
            repo: repo.clone(),
            pr,
        })
        .collect()
}

/// Verifies before decoding JSON, looking at repository fields or touching work.
/// ring's HMAC verifier compares tags in constant time.
pub fn verified(secret: &[u8], signature: Option<&str>, body: &[u8]) -> bool {
    let Some(hex) = signature.and_then(|s| s.strip_prefix("sha256=")) else {
        return false;
    };
    if hex.len() != 64 {
        return false;
    }
    let mut tag = [0; 32];
    for (i, pair) in hex.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let digit = |b: u8| match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        };
        let (Some(a), Some(b)) = (digit(pair[0]), digit(pair[1])) else {
            return false;
        };
        tag[i] = a * 16 + b;
    }
    hmac::verify(&hmac::Key::new(hmac::HMAC_SHA256, secret), body, &tag).is_ok()
}

pub struct Receiver {
    secret: Vec<u8>,
    repos: BTreeSet<String>,
    deliveries: BTreeMap<String, u64>,
    path: PathBuf,
}
impl Receiver {
    pub fn open(
        secret: Vec<u8>,
        repos: impl IntoIterator<Item = String>,
        data: &Path,
    ) -> Result<Self> {
        if secret.is_empty() {
            return Err("webhook secret is empty".into());
        }
        std::fs::create_dir_all(data)?;
        let path = data.join("deliveries.json");
        Ok(Self {
            secret,
            repos: repos.into_iter().map(|r| r.to_lowercase()).collect(),
            deliveries: persist::load(&path)?,
            path,
        })
    }
    /// Only fixed status codes escape ingress; payloads and credentials never do.
    pub fn receive(
        &mut self,
        signature: Option<&str>,
        delivery: &str,
        event: &str,
        body: &[u8],
        now: u64,
        queue: &Queue,
    ) -> u16 {
        if body.len() > MAX_BODY {
            return 413;
        }
        if !verified(&self.secret, signature, body) {
            return 401;
        }
        if delivery.is_empty() || delivery.len() > 256 {
            return 400;
        }
        self.deliveries
            .retain(|_, at| now < at.saturating_add(DELIVERY_LIFETIME));
        if self.deliveries.contains_key(delivery) {
            return 202;
        }
        let Ok(value) = serde_json::from_slice(body) else {
            return 400;
        };
        for kick in kicks(event, &value, &self.repos) {
            let queued = if event == "status" && value["id"].as_u64().is_some() {
                queue.status(&kick.repo, value["id"].as_u64().expect("checked above"))
            } else {
                queue.kick(kick)
            };
            if queued.is_err() {
                return 503;
            }
        }
        self.deliveries.insert(delivery.to_owned(), now);
        if persist::save(&self.path, &self.deliveries).is_err() {
            return 503;
        }
        202
    }
}
