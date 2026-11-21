//! The read half of a pass. The supplementary projection is behind a trait
//! because Source's REST reads do not expose resolved review threads.

use std::collections::{BTreeMap, BTreeSet};

use conary_core::Policy;
use conary_core::op::Repo;

use super::degraded::{ReadError, Reading, Reads};
use super::*;
use crate::api::{ApiError, Source};

/// GitHub facts that Source does not yet expose. The revision binds this read
/// to the REST pull read: pushes, retargets and description edits between
/// endpoints are errors, never a clean PR.
#[derive(Debug, Clone)]
pub struct BaseObservation {
    pub name: String,
    pub head: String,
    pub ci: Ci,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub revision: Revision,
    pub default_branch: Option<BaseObservation>,
    pub draft: bool,
    pub complete: bool,
    pub merge_state: String,
    pub open_threads: u32,
    pub threads: BTreeMap<String, ReviewThread>,
    pub resolved_reviews: BTreeSet<u64>,
    pub updated_at: u64,
    pub suites: BTreeMap<u64, Suite>,
    pub checks_complete: bool,
    pub rebase_removed: Vec<u64>,
    pub timeline_cut_at: Option<u64>,
    /// When the PR last changed without a push, and when its head was
    /// pushed, as the reviewers' word is read against them.
    pub since: super::reviews::Since,
}

pub trait Read: Source + crate::branches::Branches {
    fn pushes(&self, repo: &str, branch: &str) -> Result<BTreeMap<String, String>, ApiError>;
    fn snapshot(&self, repo: &str, pr: u64) -> Result<Snapshot, ApiError>;
    fn annotations(&self, repo: &str, run: u64) -> Result<Vec<String>, ApiError>;
}

/// Why a read GitHub answered with something that doesn't parse degraded.
const UNPARSED: &str = "GitHub's answer didn't parse";

/// Read once, then decide without I/O. Only the configured trusted authors
/// may enter the pipeline. Classification comes from policy, including rename
/// sources and raised labels; a PR cannot select its own class. A read it
/// can't go on without ends it, naming that read; one it goes on past, or
/// that stopped short, is in the facts' `reads`, holding what it was for
/// failed.
pub fn read(
    source: &dyn Read,
    repo: &Repo,
    pr: u64,
    policy: &Policy,
    settings: Settings,
    trusted_authors: &[String],
) -> Result<Facts, ReadError> {
    let reads = &mut Reads::default();
    match gather(source, repo, pr, policy, settings, trusted_authors, reads) {
        Ok(mut facts) => {
            facts.reads = std::mem::take(reads);
            Ok(facts)
        }
        // What the reads before it met and answered is kept with it.
        Err(mut e) => {
            e.reads = Box::new(std::mem::take(reads));
            Err(e)
        }
    }
}

/// [`read`]'s reads, each one's outcome kept in `reads`.
fn gather(
    source: &dyn Read,
    repo: &Repo,
    pr: u64,
    policy: &Policy,
    settings: Settings,
    trusted_authors: &[String],
    reads: &mut Reads,
) -> Result<Facts, ReadError> {
    let repository = repo.to_string();
    let pull = reads.ask(Reading::Pull, source.pull(&repository, pr))?;
    let extra = reads.ask(Reading::Pull, source.snapshot(&repository, pr))?;
    let revision = Revision::from(&pull);
    if extra.revision != revision {
        return Err(ReadError::with(
            Reading::Pull,
            "the PR moved between its reads".into(),
            ApiError::new("PR moved between flow reads"),
        ));
    }
    if extra.complete {
        reads.answered(Reading::Snapshot);
    } else {
        reads.met(
            Reading::Snapshot,
            "GitHub's snapshot stopped short of the PR's reviews, comments, labels or threads"
                .into(),
        );
    }
    let labels = pull.labels.iter().map(|l| l.name.clone()).collect();
    let renovate = pull.head.name.starts_with("renovate/");
    let dependency = dependency(&pull.head.name, &pull.title, &pull.labels);
    let unknown = conary_core::policy::Paths {
        files: Vec::new(),
        cut: true,
    };
    let classified = policy.classify(repo, &unknown).raise(pull.raises());
    let mut facts = Facts {
        repo: repo.clone(),
        title: pull.title.clone(),
        description: pull.body.clone().unwrap_or_default(),
        branch: pull.head.name.clone(),
        number: pull.number,
        revision,
        default_branch: pull
            .base
            .repo
            .as_ref()
            .map_or_else(String::new, |r| r.default_branch.clone()),
        open: pull.state == "open",
        draft: extra.draft,
        trusted: trusted_authors.contains(&pull.user.login),
        by_app: pull.user.kind == "Bot",
        complete: extra.complete,
        labels,
        merging: Merge::from_github(&extra.merge_state),
        checks: Vec::new(),
        stopped: BTreeMap::new(),
        timed_out: BTreeMap::new(),
        has_ci: false,
        open_threads: extra.open_threads,
        changes_requested: false,
        owner_approved: false,
        reviewers: BTreeMap::new(),
        judgment: None,
        judge_reason: None,
        judgment_generation: None,
        major_generation: None,
        jobs: BTreeMap::new(),
        claims: Vec::new(),
        stopped_jobs: BTreeSet::new(),
        unreadable: Vec::new(),
        reads: Reads::default(),
        working: BTreeSet::new(),
        paused: None,
        pushes: BTreeMap::new(),
        threads: extra.threads.clone(),
        review_bodies: BTreeMap::new(),
        judge_requests: BTreeMap::new(),
        proof: Proof::NotNeeded,
        proof_data: ProofData::default(),
        base_tip: None,
        settings,
        classified,
        authorship_known: false,
        needs_judge: false,
        judge_asked: false,
        dependency,
        renovate,
        bot_edited: None,
        gate_edited: false,
        major: None,
        release_at: None,
        rebase_requested: pull.title.starts_with("rebase!")
            || pull.labels.iter().any(|l| l.name == "rebase"),
        rebase_removed: extra.rebase_removed.clone(),
        timeline_cut_at: extra.timeline_cut_at,
        stacked_on: None,
        open_prs: BTreeSet::new(),
        queue: BTreeMap::new(),
        red_fix: super::red_fix(&pull, trusted_authors),
        base_held: false,
        queued_merge: pull.auto_merge.is_some(),
        train: TrainState::default(),
        empty: None,
        updated_at: extra.updated_at,
        created_at: pull
            .created_at
            .as_deref()
            .map(super::reviews::timestamp)
            .transpose()
            .map_err(|e| ReadError::with(Reading::Pull, UNPARSED.into(), e))?,
        last_reply_at: 0,
        session: pull
            .body
            .as_deref()
            .is_some_and(|b| b.contains("https://claude.ai/code/session_")),
        failed: None,
        succeeded: BTreeSet::new(),
    };
    // These PRs do not enter the pipeline. In particular, do not spend
    // thousands of review/comment reads after GitHub already said the cap
    // was exceeded, or fail a draft because its unfinished checks fail.
    if excluded(&facts) {
        return Ok(facts);
    }
    let open = reads.ask(
        Reading::Pulls,
        source.pulls(&repository, crate::api::Pulls::Open),
    )?;
    facts.open_prs = open.iter().map(|p| p.number).collect();
    facts.queue = open
        .iter()
        .map(|p| {
            (
                p.number,
                super::Rank::of(&facts.settings, trusted_authors, p),
            )
        })
        .collect();
    facts.stacked_on = open
        .iter()
        .find(|p| {
            p.number != pr
                && p.head.name == pull.base.name
                && p.head
                    .repo
                    .as_ref()
                    .and_then(|r| r.full_name.as_ref())
                    .zip(pull.base.repo.as_ref().and_then(|r| r.full_name.as_ref()))
                    .is_some_and(|(head, base)| head.eq_ignore_ascii_case(base))
        })
        .map(|p| p.number);
    if facts.stacked_on.is_some() {
        return Ok(facts);
    }
    let tip = reads.ask(Reading::Base, source.branch(&repository, &pull.base.name))?;
    let base = tip.as_deref().unwrap_or(&pull.base.sha);
    let paths = reads.ask(
        Reading::Files,
        source.files(&repository, base, &pull.head.sha),
    )?;
    facts.classified = policy.classify(repo, &paths).raise(pull.raises());
    let machine = facts.settings.machine();
    let mut ci = super::ci::checks(
        source,
        &repository,
        &pull.head.sha,
        &pull.base.name,
        &paths,
        facts.classified.docs,
        &extra.suites,
        &facts.settings,
        reads,
    )?;
    if !extra.checks_complete {
        reads.met(
            Reading::Checks,
            "GitHub listed more check suites than one read holds".into(),
        );
        ci.checks = vec![Check {
            name: "over 100 checks, too many to read".into(),
            state: Ci::Pending,
            run: Place::Unplaced,
        }];
        ci.docs = false;
        ci.stopped.clear();
        ci.timed_out.clear();
    }
    if facts.settings.job_login.is_some() {
        facts.pushes = reads.ask(Reading::Pushes, source.pushes(&repository, &pull.head.name))?;
    }
    let records = reads.ask(Reading::Reviews, source.reviews(&repository, pr))?;
    let comments = reads.ask(Reading::Reviews, source.comments(&repository, pr))?;
    let markers = super::markers::read(&repository, &pull, &comments, &facts.settings);
    reads.mark(super::Mark::Verdict, markers.unparsed);
    facts.changes_requested = super::reviews::changes_requested(&records);
    facts.review_bodies = super::judging::review_bodies(&records);
    facts.owner_approved =
        super::approval::from_reviews(repo, pr, &facts.revision, &records, &facts.settings.people);
    facts.reviewers = super::reviews::read(
        &pull.head.sha,
        &records,
        &comments,
        extra.since,
        &facts.settings,
        reads,
    )
    .map_err(|e| ReadError::with(Reading::Reviews, UNPARSED.into(), e))?;
    facts.judgment = markers.judgment;
    facts.judgment_generation = markers.judgment_generation;
    facts.major_generation = markers.major_generation;
    facts.judge_asked = markers.judge_asked;
    facts.major = markers.major;
    let history = reads.take(
        Reading::Commits,
        source.compare_commits(&repository, base, &pull.head.sha),
    );
    let provenance = super::history::provenance(history.as_ref(), &facts.settings, renovate, reads);
    facts.authorship_known = provenance.known;
    facts.needs_judge = provenance.judge;
    facts.bot_edited = provenance.edited;
    // A test merge of another tip or head is stale, which is GitHub's
    // answer; a read of it that fails is degraded, and the merge's effect
    // unknown.
    facts.empty = if let (Some(tip), Some(merge)) = (&tip, &pull.merge_commit_sha) {
        reads
            .take(Reading::MergeTree, source.commit(&repository, merge))
            .filter(|c| {
                c.parents
                    .iter()
                    .map(|p| p.sha.as_str())
                    .eq([tip.as_str(), pull.head.sha.as_str()])
            })
            .and_then(|_| {
                let merged = reads.take(Reading::MergeTree, source.tree(&repository, merge))?;
                let base = reads.take(Reading::MergeTree, source.tree(&repository, tip))?;
                Some(merged == base)
            })
    } else {
        None
    };
    facts.base_tip = tip.clone();
    if facts.red_fix && !facts.draft {
        // No observation of the default branch can't tell whether it's red.
        match &extra.default_branch {
            None => reads.met(
                Reading::DefaultBranch,
                "GitHub's snapshot held no observation of the default branch".into(),
            ),
            Some(_) => reads.answered(Reading::DefaultBranch),
        }
        facts.base_held = extra
            .default_branch
            .as_ref()
            .is_none_or(|b| b.ci != Ci::Red || tip.as_deref() != Some(&b.head));
    }

    facts.gate_edited = super::gates::edited(&paths, || {
        reads.take(
            Reading::Diff,
            source.diff(&repository, base, &pull.head.sha),
        )
    });
    facts.proof = if machine.is_empty() || ci.docs {
        Proof::NotNeeded
    } else if ci
        .checks
        .iter()
        .any(|c| machine.contains(&c.name.to_lowercase()) && c.state == Ci::Red)
    {
        Proof::Failed
    } else if ci
        .checks
        .iter()
        .any(|c| machine.contains(&c.name.to_lowercase()) && c.state == Ci::Pending)
    {
        Proof::Needed
    } else {
        Proof::Passed
    };
    let red: Vec<_> = ci
        .checks
        .iter()
        .filter(|c| c.state == Ci::Red)
        .map(|c| c.name.as_str())
        .collect();
    if renovate && red == ["renovate/artifacts"] {
        facts.release_at = match super::gates::release(&comments, &facts.settings) {
            Ok(at) => {
                reads.mark(super::Mark::ReleaseAge, None);
                at
            }
            Err(e) => {
                reads.mark(super::Mark::ReleaseAge, Some(e.0));
                None
            }
        };
    }
    facts.has_ci = ci.has_ci;
    facts.stopped = ci.stopped;
    facts.timed_out = ci.timed_out;
    facts.checks = ci
        .checks
        .into_iter()
        .map(|mut check| {
            if let Some((name, _)) = facts.settings.proof_recipe(&check.name) {
                check.name.clone_from(name);
            }
            check
        })
        .collect();
    Ok(facts)
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

impl Revision {
    pub fn for_parts(head: String, base: String, title: &str, body: &str) -> Self {
        Self {
            head,
            base,
            description: fingerprint(&format!("{title}\n{body}")),
        }
    }
}
impl From<&crate::api::PullRequest> for Revision {
    fn from(pull: &crate::api::PullRequest) -> Self {
        Self::for_parts(
            pull.head.sha.clone(),
            pull.base.name.clone(),
            &pull.title,
            pull.body.as_deref().unwrap_or(""),
        )
    }
}

/// A PR outside the pipeline, read no further. A held one is in it, only
/// switched off: it's read in full, so what its hold keeps off shows.
pub(crate) fn excluded(f: &Facts) -> bool {
    !f.open || f.draft || !f.trusted || !f.complete || f.stacked_on.is_some()
}
