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
