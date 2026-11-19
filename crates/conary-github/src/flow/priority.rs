//! A PR's priority: the level its labels give it, which orders proof slots,
//! train admission and a targeted kick's view of the others, and sizes how
//! long it may wait at a stage. The one place a PR's place in line is worked
//! out, so every reader orders PRs the same way.
use super::Settings;
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

/// Where a PR stands in line: by level, then age (the oldest first), then
/// number. A PR whose opening time isn't known goes after those whose is.
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
        opened: Option<u64>,
        number: u64,
    ) -> Self {
        Self {
            level: level(settings, labels),
            opened: opened.unwrap_or(u64::MAX),
            number,
        }
    }
    /// An open PR's place in line, as GitHub lists it.
    pub fn of(settings: &Settings, pull: &crate::api::PullRequest) -> Self {
        Self::new(
            settings,
            pull.labels.iter().map(|l| l.name.as_str()),
            pull.created_at
                .as_deref()
                .and_then(|at| super::reviews::timestamp(at).ok()),
            pull.number,
        )
    }
}
