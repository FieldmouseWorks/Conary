//! A PR's priority: the level its labels give it, which orders proof slots,
//! train admission and a targeted kick's view of the others, and sizes how
//! long it may wait at a stage. The one place a PR's place in line is worked
//! out, so every reader orders PRs the same way.
use super::Settings;
use crate::api::PullRequest;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// A priority level, the levels burrow's drain uses. Declared highest first,
/// so ordering by level puts the highest first.
#[derive(
    Debug,
    Default,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    High,
    #[default]
    Medium,
    Low,
}
impl Level {
    const ALL: [Self; 3] = [Self::High, Self::Medium, Self::Low];
}

/// Seconds a PR may wait at one stage before it's overdue, by its level.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct StallAfter {
    /// Seconds a `high` PR may wait at one stage before it's overdue.
    #[schemars(extend("x-spender" = false))]
    pub high: u64,
    /// Seconds a `medium` PR may wait at one stage before it's overdue.
    #[schemars(extend("x-spender" = false))]
    pub medium: u64,
    /// Seconds a `low` PR may wait at one stage before it's overdue.
    #[schemars(extend("x-spender" = false))]
    pub low: u64,
}
impl StallAfter {
    pub fn at(&self, level: Level) -> u64 {
        match level {
            Level::High => self.high,
            Level::Medium => self.medium,
            Level::Low => self.low,
        }
    }
}
impl Default for StallAfter {
    /// Owner-expedited work is overdue in half an hour, the rest in two
    /// hours (burrow#960).
    fn default() -> Self {
        Self {
            high: 1800,
            medium: 7200,
            low: 7200,
        }
    }
}

/// The level `labels` give a PR: the highest of those a label of theirs is
/// mapped to in `priority_labels`, or the default level if none is.
pub fn level<'a>(settings: &Settings, labels: impl IntoIterator<Item = &'a str>) -> Level {
    let theirs: BTreeSet<&str> = labels.into_iter().collect();
    Level::ALL
        .into_iter()
        .find(|level| {
            settings
                .priority_labels
                .iter()
                .any(|(label, mapped)| mapped == level && theirs.contains(label.as_str()))
        })
        .unwrap_or(settings.default_priority)
}

/// The branch a red default branch's fix comes from, the red's commit after
/// it: the steward's red jobs push there, and so does anyone of its own who
/// fixes a red by hand (house/prs.md).
pub const RED_FIX_BRANCH: &str = "autoland/red-";

/// Whether the PR is a fix for its repository's red default branch: from a
/// [`RED_FIX_BRANCH`] in the repository itself, by a trusted author, into the
/// default branch. Anyone can open a PR from a branch of that name on a
/// fork, and that one is no fix of the pipeline's. Every PR is built on the
/// red branch until a fix lands, so a fix goes first in line ([`Rank`]) and
/// waits on none of the red branch's own runs (the read's base hold). Draft
/// or not: a draft never waits for a slot or joins the train, so its place
/// in line orders nothing, and the base hold leaves drafts out itself.
pub fn red_fix(pull: &PullRequest, trusted_authors: &[String]) -> bool {
    let own_branch = pull
        .head
        .repo
        .as_ref()
        .and_then(|r| r.full_name.as_ref())
        .zip(pull.base.repo.as_ref().and_then(|r| r.full_name.as_ref()))
        .is_some_and(|(head, base)| head.eq_ignore_ascii_case(base));
    pull.head.name.starts_with(RED_FIX_BRANCH)
        && own_branch
        && pull.targets_trunk()
        && trusted_authors.contains(&pull.user.login)
}

/// Where a PR stands in line: by level, then age (the oldest first), then
/// number. A PR whose opening time isn't known goes after those whose is.
/// A red default branch's fix ([`red_fix`]) is `high` whatever its labels,
/// so it needs no one to label it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Rank {
    pub level: Level,
    pub opened: u64,
    pub number: u64,
}
impl Rank {
    pub fn new<'a>(
        settings: &Settings,
        labels: impl IntoIterator<Item = &'a str>,
        red_fix: bool,
        opened: Option<u64>,
        number: u64,
    ) -> Self {
        Self {
            level: if red_fix {
                Level::High
            } else {
                level(settings, labels)
            },
            opened: opened.unwrap_or(u64::MAX),
            number,
        }
    }
    /// An open PR's place in line, as GitHub lists it.
    pub fn of(settings: &Settings, trusted_authors: &[String], pull: &PullRequest) -> Self {
        Self::new(
            settings,
            pull.labels.iter().map(|l| l.name.as_str()),
            red_fix(pull, trusted_authors),
            pull.created_at
                .as_deref()
                .and_then(|at| super::reviews::timestamp(at).ok()),
            pull.number,
        )
    }
}
