//! A proof's deadline, sized from its own history (`limits-are-design-bugs`).
//!
//! Each proof that answered with every check it ran passing is timed, with
//! the recipes it ran and whether it held a proof slot, in one file of the
//! flow's own records for the repository ([`Durations`]). A proof's deadline
//! is [`MARGIN`] times the longest of the last [`KEEP`] such runs of the same
//! recipes held the same way, never below [`FLOOR`] and never above the
//! configured `proof_timeout`. With no such run yet, or once one of its
//! checks reached a deadline on the PR as its recipe stands, it's
//! `proof_timeout`. Only a real pass writes the file, under the
//! repository's lock; a dry run reads it as it is.
use super::persist::{self, Result};
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The runs kept for each recipe set, held one way: the newest.
pub const KEEP: usize = 10;
/// A sized deadline is this many times the longest kept run.
pub const MARGIN: u64 = 2;
/// The shortest deadline history sizes: fifteen minutes.
pub const FLOOR: u64 = 900;

/// The repository's timed proofs: the newest [`KEEP`] of each recipe set,
/// held one way, newest first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Durations {
    #[serde(default)]
    pub runs: Vec<Run>,
}
/// One proof that answered with every check it ran passing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    /// The checks it ran, each with its recipe.
    pub recipes: BTreeMap<String, Recipe>,
    /// Whether it held a proof slot: a proof its preparation said needs
    /// none is timed apart, as it may run on inputs already proved.
    pub slot: bool,
    pub job: String,
    /// From its start to its answer.
    pub seconds: u64,
    /// When it answered.
    pub at: u64,
}
/// A proof's deadline, in seconds from its start, and where it came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sized {
    pub seconds: u64,
    pub from: Source,
}
impl Sized {
    /// The configured `proof_timeout`, as every proof had before the flow
    /// sized them.
    pub fn configured(seconds: u64) -> Self {
        Self {
            seconds,
            from: Source::Configured,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// `proof_timeout`: these recipes, held this way, have no timed run yet.
    Configured,
    /// `proof_timeout`: one of its checks reached a deadline on this PR as
    /// its recipe stands, so history said too little for it.
    Reached,
    /// Sized from this many runs, the longest this many seconds.
    History { runs: usize, longest: u64 },
}

const FILE: &str = "durations.json";
/// Where a repository's flow directory keeps its timed proofs.
pub fn path(dir: &Path) -> PathBuf {
    dir.join(FILE)
}
/// The repository's timed proofs, as the last real pass saved them: none
/// when there's no file yet. A file that doesn't read is an error, never
/// taken as no history.
pub(crate) fn read(root: &Path, repo: &conary_core::op::Repo) -> Result<Durations> {
    persist::load(&path(&persist::dir(root, repo)))
}
impl Durations {
    /// Adds each run not kept yet and keeps the newest [`KEEP`] of each
    /// recipe set, held one way. Whether anything changed. A run read again
    /// after it fell out is older than every one kept, so it falls out
    /// again: folding the same runs twice changes nothing.
    pub(crate) fn fold(&mut self, ran: &[Run]) -> bool {
        let before = self.runs.clone();
        for run in ran {
            if !self.runs.iter().any(|r| r.job == run.job) {
                self.runs.push(run.clone());
            }
        }
        self.runs
            .sort_by(|a, b| (b.at, &b.job).cmp(&(a.at, &a.job)));
        let mut kept: BTreeMap<(&BTreeMap<String, Recipe>, bool), usize> = BTreeMap::new();
        let keep: Vec<bool> = self
            .runs
            .iter()
            .map(|r| {
                let n = kept.entry((&r.recipes, r.slot)).or_default();
                *n += 1;
                *n <= KEEP
            })
            .collect();
        let mut keep = keep.into_iter();
        self.runs.retain(|_| keep.next().unwrap_or(false));
        self.runs != before
    }
}
/// Gives a decision the repository's timed proofs, or why they didn't read.
pub(crate) fn apply(f: &mut Facts, durations: &std::result::Result<Durations, String>) {
    match durations {
        Ok(timed) => f.proof_data.durations = timed.clone(),
        Err(why) => f.proof_data.unread = Some(why.clone()),
    }
}
/// Saves the repository's timed proofs, under its lock.
pub(crate) fn save(data: &Data, durations: &Durations) -> Result<()> {
    persist::save(&path(data.dir()), durations)
}

/// The timed run a proof's answer makes, when it answered with every check
/// it was asked for passing.
pub(crate) fn ran(work: &jobs::Work, completion: &jobs::Completion) -> Option<Run> {
    use conary_core::plugin::{Answer, Response, Verdict};
    let Some(Ok(Response {
        answer: Answer::Proof { results, .. },
        ..
    })) = &completion.result
    else {
        return None;
    };
    let passed = !work.recipes.is_empty()
        && work.recipes.keys().all(|check| {
            results
                .iter()
                .any(|r| &r.check == check && matches!(r.verdict, Verdict::Pass))
        });
    (matches!(work.purpose, jobs::Purpose::Proof) && passed).then(|| Run {
        recipes: work.recipes.clone(),
        slot: !work.slotless,
        job: work.job.id.clone(),
        seconds: completion.at.saturating_sub(work.job.started),
        at: completion.at,
    })
}

/// The deadline the proof this decision would start gets, and where it
/// came from.
pub(crate) fn sized(f: &Facts) -> Sized {
    let timeout = f.settings.proof_timeout;
    let recipes = super::proofs::selected(f);
    if f.proof_data
        .deadlines
        .iter()
        .any(|r| recipes.get(&r.check) == Some(&r.recipe))
    {
        return Sized {
            seconds: timeout,
            from: Source::Reached,
        };
    }
    let slot = super::proofs::needs_slot(f);
    let runs: Vec<u64> = f
        .proof_data
        .durations
        .runs
        .iter()
        .filter(|r| r.recipes == recipes && r.slot == slot)
        .map(|r| r.seconds)
        .collect();
    match runs.iter().max() {
        None => Sized::configured(timeout),
        Some(&longest) => Sized {
            seconds: longest.saturating_mul(MARGIN).max(FLOOR).min(timeout),
            from: Source::History {
                runs: runs.len(),
                longest,
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(job: &str, recipe: &str, at: u64) -> Run {
        Run {
            recipes: [("machine".to_string(), Recipe::Run(vec![recipe.into()]))].into(),
            slot: true,
            job: job.into(),
            seconds: 600,
            at,
        }
    }

    /// The newest [`KEEP`] of each recipe set are kept, and folding the
    /// same runs again, those that fell out among them, changes nothing.
    #[test]
    fn folding_keeps_the_newest_and_is_idempotent() {
        let all: Vec<Run> = (0..KEEP as u64 + 3)
            .map(|i| run(&format!("j{i}"), "check", i))
            .chain([run("other", "other", 0)])
            .collect();
        let mut d = Durations::default();
        assert!(d.fold(&all));
        assert_eq!(d.runs.len(), KEEP + 1);
        assert!(d.runs.iter().all(|r| r.job != "j0" && r.job != "j2"));
        assert!(d.runs.iter().any(|r| r.job == "other"));
        assert!(!d.fold(&all));
        assert!(!d.fold(&all[..3]));
        let newer = run("new", "check", 100);
        assert!(d.fold(std::slice::from_ref(&newer)));
        assert_eq!(d.runs[0], newer);
        assert!(d.runs.iter().all(|r| r.job != "j3"));
    }
}
