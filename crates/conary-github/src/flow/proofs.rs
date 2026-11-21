//! Local proof observations. Recipe identity and head bind every verdict.
//! A proof is a program: the proof plugin first prepares its inputs, holding
//! no proof slot, then proves, holding one unless its preparation said it
//! needs none. A model-run check is never sent to the proof plugin.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProofData {
    /// Capacity without ownership. It must never enter job cancellation.
    pub reserved: u32,
    /// The PR whose work can't be told [`Self::reserved`] is for, and why.
    pub reserved_by: Option<(u64, &'static str)>,
    pub records: Vec<ProofRecord>,
    pub live: Vec<ProofHolder>,
    pub waiting: Vec<ProofWaiting>,
    /// This PR's preparations, in the order they started.
    pub prepared: Vec<Preparation>,
    /// Every deadline this PR's proof-plugin work reached, per check.
    pub deadlines: Vec<Reached>,
    /// The head's tree: read only once a selected check has reached its
    /// deadline twice on one tree ([`tree`]), and None when that read
    /// failed.
    pub tree: Option<String>,
}
/// A check whose preparation or proof reached its deadline
/// ([`super::jobs::Work::reached_deadline`]): with which recipe, on which
/// tree, and when that deadline was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reached {
    pub check: String,
    pub recipe: Recipe,
    pub tree: String,
    pub at: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProofRecord {
    pub job: Job,
    pub recipes: BTreeMap<String, Recipe>,
    /// None is an infrastructure failure, even when the unit exited zero.
    pub verdicts: Option<BTreeMap<String, Verdict>>,
    pub note_needed: bool,
}
/// A proof that holds one of the scarce proof slots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProofHolder {
    pub number: u64,
    pub job: Job,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProofWaiting {
    pub number: u64,
}
/// One `prepare` invocation for a head and recipe set. It holds no proof
/// slot, and its answer is never a verdict: only whether proving may start
/// and whether it needs a slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preparation {
    pub job: Job,
    pub recipes: BTreeMap<String, Recipe>,
    /// When it answered or failed; None while it runs.
    pub ended: Option<u64>,
    /// Its answer. None while it runs, and when it failed or answered
    /// anything but readiness.
    pub answer: Option<Readiness>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Readiness {
    pub ready: bool,
    /// Whether proving needs one of the scarce proof slots.
    pub slot: bool,
    pub why: String,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ProofPlan {
    /// Ready these recipes' inputs, holding no proof slot.
    Prepare(BTreeMap<String, Recipe>),
    /// Run these recipes, holding a proof slot when `slot` is set.
    Prove {
        recipes: BTreeMap<String, Recipe>,
        slot: bool,
    },
    Post {
        job: String,
        verdicts: BTreeMap<String, Verdict>,
        note: bool,
    },
}

/// A failed or unready preparation is asked again after this long.
const PREPARE_AGAIN: u64 = 900;
/// The deadlines a check reaches on one tree, as its recipe stands, before
/// the flow stops starting it there: the first is retried, the second is a
/// stall.
const STALL_AT: usize = 2;

/// Why no proof-plugin work starts for the selected checks: a deadline hit
/// over and over is a stall to fix, not a retry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Stall {
    /// `check` reached its deadline twice on `tree`, the head's, at
    /// `first` and then `second`. Overdue at once.
    Twice {
        check: String,
        tree: String,
        first: u64,
        second: u64,
    },
    /// `check` reached its deadline twice on a tree, and the head's tree
    /// couldn't be read, so whether it's that tree can't be told. The read
    /// is named degraded; nothing starts until it answers.
    Untold { check: String },
}
impl std::fmt::Display for Stall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Twice {
                check,
                tree,
                first,
                second,
            } => write!(
                f,
                "Stalled: {check} reached its deadline twice on tree {}, at {first} and {second}; not started again",
                tree.get(..12).unwrap_or(tree)
            ),
            Self::Untold { check } => write!(
                f,
                "Can't read the head's tree to tell if {check} reached its deadline twice on it; not started"
            ),
        }
    }
}
/// The deadlines each selected check reached, as its recipe stands, by
/// tree, in the order they fell.
fn reached(f: &Facts) -> Vec<(String, BTreeMap<&str, Vec<u64>>)> {
    selected(f)
        .into_iter()
        .map(|(check, recipe)| {
            let mut trees: BTreeMap<&str, Vec<u64>> = BTreeMap::new();
            for r in &f.proof_data.deadlines {
                if r.check == check && r.recipe == recipe {
                    trees.entry(r.tree.as_str()).or_default().push(r.at);
                }
            }
            for ats in trees.values_mut() {
                ats.sort_unstable();
            }
            (check, trees)
        })
        .collect()
}
/// Whether some selected check has reached its deadline twice on one tree,
/// so the head's tree decides whether it's stalled.
fn needs_tree(f: &Facts) -> bool {
    reached(f)
        .iter()
        .any(|(_, trees)| trees.values().any(|ats| ats.len() >= STALL_AT))
}
/// Reads the head's tree when [`needs_tree`], through the degraded door: a
/// read that fails is named, and leaves it None, which [`stalled`] can't
/// tell from the stalled tree. Every decision of the PR's, its own pass's
/// and another's proof queue, reads it the same way.
pub(crate) fn tree<S: crate::branches::Branches + ?Sized>(source: &S, f: &mut Facts) {
    if !needs_tree(f) {
        return;
    }
    let repo = f.repo.to_string();
    f.proof_data.tree = f
        .reads
        .take(Reading::Tree, source.tree(&repo, &f.revision.head));
}
/// The selected checks' stall, if one has reached its deadline twice on the
/// head's tree. A new tree or a changed recipe starts the count again; a new
/// head on the same tree doesn't.
pub(super) fn stalled(f: &Facts) -> Option<Stall> {
    for (check, trees) in reached(f) {
        if !trees.values().any(|ats| ats.len() >= STALL_AT) {
            continue;
        }
        let Some(tree) = &f.proof_data.tree else {
            return Some(Stall::Untold { check });
        };
        if let Some(ats) = trees.get(tree.as_str()).filter(|a| a.len() >= STALL_AT) {
            return Some(Stall::Twice {
                check,
                tree: tree.clone(),
                first: ats[0],
                second: ats[1],
            });
        }
    }
    None
}

fn current(f: &Facts, r: &ProofRecord) -> bool {
    r.job.revision.head == f.revision.head
}
fn remaining(f: &Facts, r: &ProofRecord) -> BTreeMap<String, Verdict> {
    r.verdicts
        .iter()
        .flatten()
        .filter(|(name, _)| {
            f.settings.recipe(name) == r.recipes.get(*name)
                && f.checks
                    .iter()
                    .any(|c| c.name.eq_ignore_ascii_case(name) && c.state == Ci::Pending)
        })
        .map(|(k, v)| (k.clone(), *v))
        .collect()
}
/// Leftovers are flushed even after GitHub accepted another failure.
pub(super) fn posting(f: &Facts) -> Option<ProofPlan> {
    f.proof_data
        .records
        .iter()
        .filter(|r| current(f, r) && !r.job.state.live())
        .find_map(|r| {
            let verdicts = remaining(f, r);
            (!verdicts.is_empty()).then(|| ProofPlan::Post {
                job: r.job.id.clone(),
                verdicts,
                note: r.note_needed,
            })
        })
}
fn pending(f: &Facts) -> impl Iterator<Item = (&String, &Recipe)> {
    f.checks
        .iter()
        .filter(|c| c.state == Ci::Pending)
        .filter_map(|c| f.settings.proof_recipe(&c.name))
}
/// The pending programs the next proof runs: builds before commands. A
/// model-run check is never among them.
pub(crate) fn selected(f: &Facts) -> BTreeMap<String, Recipe> {
    for rank in [
        (|r: &Recipe| matches!(r, Recipe::Build(_))) as fn(&Recipe) -> bool,
        |r| matches!(r, Recipe::Run(_)),
    ] {
        let selected: BTreeMap<_, _> = pending(f)
            .filter(|(_, recipe)| rank(recipe))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if !selected.is_empty() {
            return selected;
        }
    }
    BTreeMap::new()
}
/// The pending checks a model would run, which no proof plugin is sent.
fn model_run(f: &Facts) -> BTreeSet<&String> {
    pending(f)
        .filter(|(_, recipe)| matches!(recipe, Recipe::Job(_)))
        .map(|(name, _)| name)
        .collect()
}

/// Whether this decision's proof holds a slot: every proof but one its
/// preparation said needs none.
pub(crate) fn slotted(next: &Next) -> bool {
    next.step == Step::Prove && !matches!(next.proof, Some(ProofPlan::Prove { slot: false, .. }))
}

/// Whose work, untold, holds every slot ([`ProofData::reserved_by`]), as a
/// slot wait says it.
pub(super) fn held_by(f: &Facts) -> String {
    f.proof_data
        .reserved_by
        .map(|(by, why)| format!(": #{by}'s {why}, so every slot is held"))
        .unwrap_or_default()
}

/// Number of slot holders and waiters ahead. Only open PRs and waiters
/// ahead in line ([`super::Rank`]) in this pass count.
pub(super) fn ahead(f: &Facts) -> u32 {
    let Some(slots) = f.settings.proof_slots else {
        return 0;
    };
    let mut count = f.proof_data.reserved.saturating_add(
        f.proof_data
            .live
            .iter()
            .filter(|h| f.open_prs.contains(&h.number) && h.job.state.live())
            .count() as u32,
    );
    let own = f.rank();
    count = count.saturating_add(
        f.proof_data
            .waiting
            .iter()
            .filter(|w| {
                w.number != f.number
                    && f.open_prs.contains(&w.number)
                    && f.queue.get(&w.number).is_some_and(|theirs| *theirs < own)
            })
            .count() as u32,
    );
    if count < slots { 0 } else { count }
}

pub(super) fn retry(f: &Facts, m: &mut Memory, now: u64) -> Option<super::decide::Decision> {
    let recipes = selected(f);
    let record = f.proof_data.records.iter().rev().find(|r| {
        current(f, r)
            && recipes
                .iter()
                .all(|(name, recipe)| r.recipes.get(name) == Some(recipe))
    });
    let mut observed = f.clone();
    if !f.proof_data.records.is_empty() {
        observed.jobs.remove(&Kind::Proof);
    }
    if let Some(record) = record {
        if record.job.state.live() {
            return None;
        }
        let mut job = record.job.clone();
        if record.verdicts.is_none() {
            job.state = JobState::Broken;
        }
        observed.jobs.insert(Kind::Proof, job);
    }
    if observed.jobs.contains_key(&Kind::Proof) {
        let decision = super::decide::work(&observed, m, now, Kind::Proof);
        if decision.1 != Step::Prove {
            return Some(decision);
        }
    }
    None
}

/// Examine every live proof, not just the single job used for the normal
/// progress status. An audit record for a newer job cannot hide older work.
pub(super) fn obsolete(f: &Facts) -> Option<&Job> {
    let wanted = selected(f);
    f.proof_data
        .records
        .iter()
        .filter(|r| {
            r.job.state.live()
                && (r.job.revision.head != f.revision.head
                    || r.recipes
                        .iter()
                        .any(|(name, recipe)| f.settings.recipe(name) != Some(recipe))
                    || !wanted.is_empty()
                        && !wanted
                            .iter()
                            .all(|(name, recipe)| r.recipes.get(name) == Some(recipe)))
        })
        .map(|r| &r.job)
        .chain(
            f.proof_data
                .live
                .iter()
                .filter(|h| {
                    h.number == f.number
                        && h.job.state.live()
                        && h.job.revision.head != f.revision.head
                })
                .map(|h| &h.job),
        )
        .chain(
            f.jobs
                .get(&Kind::Proof)
                .filter(|j| j.state.live() && j.revision.head != f.revision.head),
        )
        .min_by(|a, b| a.started.cmp(&b.started).then_with(|| a.id.cmp(&b.id)))
}

/// A running preparation is cancelled once its head or a selected recipe
/// changed: it prepares for nothing the flow will prove.
pub(super) fn obsolete_preparation(f: &Facts) -> Option<&Job> {
    let wanted = selected(f);
    f.proof_data
        .prepared
        .iter()
        .filter(|p| {
            p.job.state.live() && (p.job.revision.head != f.revision.head || p.recipes != wanted)
        })
        .map(|p| &p.job)
        .min_by(|a, b| a.started.cmp(&b.started).then_with(|| a.id.cmp(&b.id)))
}

/// The latest preparation for this head and recipe set. One runs at a time.
fn preparation<'a>(f: &'a Facts, recipes: &BTreeMap<String, Recipe>) -> Option<&'a Preparation> {
    f.proof_data
        .prepared
        .iter()
        .rev()
        .find(|p| p.job.revision.head == f.revision.head && &p.recipes == recipes)
}
/// Whether proving the selected recipes holds a proof slot: always, unless
/// their preparation answered ready and said it needs none.
pub(super) fn needs_slot(f: &Facts) -> bool {
    !preparation(f, &selected(f))
        .and_then(|p| p.answer.as_ref())
        .is_some_and(|r| r.ready && !r.slot)
}

fn deadline(started: u64, timeout: u64, now: u64) -> u64 {
    let deadline = started.saturating_add(timeout + 300);
    if now < deadline {
        deadline
    } else {
        now.saturating_add(900)
    }
}

pub(super) fn next(f: &Facts, now: u64) -> Option<(Stage, Step, String, Option<ProofPlan>)> {
    let recipes = selected(f);
    if recipes.is_empty() {
        let jobs = model_run(f);
        return (!jobs.is_empty()).then(|| {
            (
                Stage::Proof,
                Step::Wait { until: None },
                format!(
                    "Waiting: {} is a model-run check, not a proof, so no proof plugin runs it",
                    jobs.into_iter().cloned().collect::<Vec<_>>().join(", ")
                ),
                None,
            )
        });
    }
    let selected_names: BTreeSet<_> = recipes.keys().collect();
    for r in &f.proof_data.records {
        if !current(f, r) || !r.job.state.live() {
            continue;
        }
        let names: BTreeSet<_> = r.recipes.keys().collect();
        let fits = selected_names.is_subset(&names)
            && recipes.iter().all(|(k, v)| r.recipes.get(k) == Some(v));
        return Some(if fits {
            (
                Stage::Proof,
                Step::Wait {
                    until: Some(deadline(r.job.started, f.settings.proof_timeout, now)),
                },
                "Machine proof is still running".into(),
                None,
            )
        } else {
            (
                Stage::Proof,
                Step::StopJob(r.job.id.clone()),
                "Proof recipe changed; stop the old proof".into(),
                None,
            )
        });
    }
    let preparation = preparation(f, &recipes);
    let prepare = || {
        Some((
            Stage::Proof,
            Step::Prepare,
            "Prepare the proof's inputs, outside the proof slots".into(),
            Some(ProofPlan::Prepare(recipes.clone())),
        ))
    };
    let Some(preparation) = preparation else {
        return prepare();
    };
    if preparation.job.state.live() {
        return Some((
            Stage::Proof,
            Step::Wait {
                until: Some(deadline(
                    preparation.job.started,
                    f.settings.proof_timeout,
                    now,
                )),
            },
            "Preparing the proof's inputs, outside the proof slots".into(),
            None,
        ));
    }
    let slot = match &preparation.answer {
        Some(Readiness {
            ready: true, slot, ..
        }) => *slot,
        answer => {
            let again = preparation
                .ended
                .unwrap_or(preparation.job.started)
                .saturating_add(PREPARE_AGAIN);
            if now >= again {
                return prepare();
            }
            let why = match answer {
                Some(r) => format!("Proof inputs aren't ready: {}", r.why),
                None => "Proof preparation failed".into(),
            };
            return Some((
                Stage::Proof,
                Step::Wait { until: Some(again) },
                format!("{why}; prepare again after fifteen minutes")
                    .chars()
                    .take(140)
                    .collect(),
                None,
            ));
        }
    };
    if slot {
        let ahead = ahead(f);
        if ahead > 0 {
            return Some((
                Stage::ProofSlot,
                Step::Wait { until: None },
                format!(
                    "Waiting for a machine proof slot ({ahead} ahead{})",
                    held_by(f)
                ),
                None,
            ));
        }
    }
    Some((
        Stage::Proof,
        Step::Prove,
        if slot {
            "Start the next machine proof"
        } else {
            "Start the proof; its preparation says it needs no proof slot"
        }
        .into(),
        Some(ProofPlan::Prove { recipes, slot }),
    ))
}
