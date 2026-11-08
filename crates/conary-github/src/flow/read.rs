//! The read half of a pass. The supplementary projection is behind a trait
//! because Source's REST reads do not expose resolved review threads.

use std::collections::BTreeMap;

use conary_core::Policy;
use conary_core::op::Repo;

use super::*;
use crate::api::{ApiError, Source};

/// GitHub facts that Source does not yet expose. The head binds this read
/// to the REST pull read: a push between endpoints is an error, not a clean PR.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub head: String,
    pub draft: bool,
    pub complete: bool,
    pub merge_state: String,
    pub open_threads: u32,
    pub changes_requested: bool,
    pub reviewers: BTreeMap<String, Reviewer>,
    pub updated_at: u64,
}

pub trait Read: Source {
    fn snapshot(&self, repo: &str, pr: u64) -> Result<Snapshot, ApiError>;
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
    if extra.head != pull.head.sha {
        return Err(ApiError::new("PR moved between flow reads"));
    }
    let paths = source.files(&repository, &pull.base.sha, &pull.head.sha)?;
    let classified = policy.classify(repo, &paths).raise(pull.raises());
    let checks = checks(source, &repository, &pull.head.sha)?;
    let labels = pull.labels.iter().map(|l| l.name.clone()).collect();
    let renovate = pull.head.name.starts_with("renovate/");
    let dependency = dependency(&pull.head.name, &pull.title, &pull.labels);
    Ok(Facts {
        revision: Revision {
            head: pull.head.sha,
            base: pull.base.name,
            description: format!("{}\n{}", pull.title, pull.body.as_deref().unwrap_or("")),
        },
        open: pull.state == "open",
        draft: extra.draft,
        trusted: trusted_authors.contains(&pull.user.login),
        complete: extra.complete,
        labels,
        merging: Merge::from_github(&extra.merge_state),
        checks,
        open_threads: extra.open_threads,
        changes_requested: extra.changes_requested,
        reviewers: extra.reviewers,
        judgment: None,
        jobs: BTreeMap::new(),
        proof: if settings.proofs.is_empty() {
            Proof::NotNeeded
        } else {
            Proof::Needed
        },
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

fn checks(source: &dyn Read, repo: &str, head: &str) -> Result<Vec<Check>, ApiError> {
    // Reruns replace earlier runs of the same check; status contexts are
    // case-insensitive and returned newest first by GitHub.
    let mut runs = BTreeMap::new();
    for run in source.check_runs(repo, head)? {
        let keep = runs
            .get(&run.name)
            .is_none_or(|old: &crate::api::CheckRun| old.id < run.id);
        if keep {
            runs.insert(run.name.clone(), run);
        }
    }
    let mut checks = Vec::new();
    for run in runs.into_values() {
        let state = if run.status != "completed" {
            Ci::Pending
        } else {
            match run.conclusion.as_deref() {
                Some("success" | "neutral" | "skipped") => Ci::Green,
                Some("cancelled") => Ci::Pending,
                _ => Ci::Red,
            }
        };
        checks.push(Check {
            name: run.name,
            state,
        });
    }
    let mut seen = std::collections::BTreeSet::new();
    for status in source.statuses(repo, head)? {
        let name = status.context.to_lowercase();
        if !seen.insert(name.clone()) || name == "review-settled" {
            continue;
        }
        if name.starts_with(crate::train::CONTEXT)
            && (status.state != "failure"
                || !status
                    .creator
                    .as_ref()
                    .is_some_and(|u| u.login == "fieldmouseworks-steward[bot]"))
        {
            continue;
        }
        let state = match status.state.as_str() {
            "success" => Ci::Green,
            "pending" | "expected" => Ci::Pending,
            _ => Ci::Red,
        };
        checks.push(Check {
            name: status.context,
            state,
        });
    }
    Ok(checks)
}
