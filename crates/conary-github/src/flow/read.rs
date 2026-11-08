//! The read half of a pass. The supplementary projection is behind a trait
//! because Source's REST reads do not expose resolved review threads.

use std::collections::BTreeMap;

use conary_core::Policy;
use conary_core::op::Repo;

use super::*;
use crate::api::{ApiError, Source};

/// GitHub facts that Source does not yet expose. The revision binds this read
/// to the REST pull read: pushes, retargets and description edits between
/// endpoints are errors, never a clean PR.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub revision: Revision,
    pub draft: bool,
    pub complete: bool,
    pub merge_state: String,
    pub open_threads: u32,
    pub changes_requested: bool,
    pub reviewers: BTreeMap<String, Reviewer>,
    pub updated_at: u64,
    pub suites: BTreeMap<u64, Suite>,
}

pub trait Read: Source + crate::branches::Branches {
    fn snapshot(&self, repo: &str, pr: u64) -> Result<Snapshot, ApiError>;
    fn annotations(&self, repo: &str, run: u64) -> Result<Vec<String>, ApiError>;
}

/// Read once, then decide without I/O. Only the configured trusted authors
/// may enter the pipeline. Classification comes from policy, including rename
/// sources and raised labels; a PR cannot select its own class.
pub fn read(
    source: &dyn Read,
    repo: &Repo,
    pr: u64,
    policy: &Policy,
    settings: Settings,
    trusted_authors: &[String],
) -> Result<Facts, ApiError> {
    let repository = repo.to_string();
    let pull = source.pull(&repository, pr)?;
    let extra = source.snapshot(&repository, pr)?;
    let revision = Revision::from(&pull);
    if extra.revision != revision {
        return Err(ApiError::new("PR moved between flow reads"));
    }
    let paths = source.files(&repository, &pull.base.sha, &pull.head.sha)?;
    let classified = policy.classify(repo, &paths).raise(pull.raises());
    let machine = settings.machine();
    let ci = super::ci::checks(
        source,
        &repository,
        &pull.head.sha,
        &pull.base.name,
        &paths,
        &extra.suites,
        &machine,
    )?;
    let labels = pull.labels.iter().map(|l| l.name.clone()).collect();
    let renovate = pull.head.name.starts_with("renovate/");
    let dependency = dependency(&pull.head.name, &pull.title, &pull.labels);
    let proof = if machine.is_empty() || ci.docs {
        Proof::NotNeeded
    } else if ci
        .checks
        .iter()
        .any(|c| machine.contains(&c.name) && c.state == Ci::Red)
    {
        Proof::Failed
    } else if ci
        .checks
        .iter()
        .any(|c| machine.contains(&c.name) && c.state == Ci::Pending)
    {
        Proof::Needed
    } else {
        Proof::Passed
    };
    Ok(Facts {
        revision,
        default_branch: pull
            .base
            .repo
            .as_ref()
            .map_or_else(String::new, |r| r.default_branch.clone()),
        open: pull.state == "open",
        draft: extra.draft,
        trusted: trusted_authors.contains(&pull.user.login),
        complete: extra.complete,
        labels,
        merging: Merge::from_github(&extra.merge_state),
        checks: ci.checks,
        open_threads: extra.open_threads,
        changes_requested: extra.changes_requested,
        reviewers: extra.reviewers,
        judgment: None,
        jobs: BTreeMap::new(),
        proof,
        settings,
        classified,
        unseen: false,
        // Commit provenance has not been read by this adapter. A missing read
        // is never permission to land on reviews alone.
        authorship_known: false,
        needs_judge: false,
        judge_asked: false,
        dependency,
        renovate,
        bot_edited: None,
        gate_edited: false,
        major: None,
        release_at: None,
        bot_merges: renovate,
        stacked_on: None,
        base_held: false,
        empty: None,
        updated_at: extra.updated_at,
        last_reply_at: 0,
        session: pull
            .body
            .as_deref()
            .is_some_and(|b| b.contains("https://claude.ai/code/session_")),
        failed: None,
    })
}

fn dependency(branch: &str, title: &str, labels: &[crate::api::Label]) -> Option<Dependency> {
    if !branch.starts_with("renovate/") && !branch.starts_with("dependabot/") {
        return None;
    }
    let label = |s| labels.iter().any(|l| l.name == s);
    if branch.starts_with("renovate/") && label("security") {
        return Some(Dependency::Minor);
    }
    let versions = regex::Regex::new(r"from v?(\d+)\S* to v?(\d+)").expect("constant regex");
    let major = label("major") || versions.captures(title).is_some_and(|c| c[1] != c[2]);
    Some(if major {
        Dependency::Major
    } else {
        Dependency::Minor
    })
}

impl From<&crate::api::PullRequest> for Revision {
    fn from(pull: &crate::api::PullRequest) -> Self {
        Self {
            head: pull.head.sha.clone(),
            base: pull.base.name.clone(),
            description: format!("{}\n{}", pull.title, pull.body.as_deref().unwrap_or("")),
        }
    }
}
