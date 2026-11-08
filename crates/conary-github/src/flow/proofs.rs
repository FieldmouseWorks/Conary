//! Local proof observations. Recipe identity and head bind every verdict;
//! store paths describe work shared between proofs, never evidence for a
//! different derivation. Evaluation and execution remain caller effects.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Default)]
pub struct ProofData {
    pub evaluation: Option<Evaluation>,
    pub records: Vec<ProofRecord>,
    pub live: Vec<ProofHolder>,
    pub waiting: Vec<ProofWaiting>,
    pub valid_paths: BTreeSet<String>,
    pub inputs: BTreeSet<String>,
    pub inputs_job: Option<InputBuild>,
}
#[derive(Debug, Clone)]
pub struct InputBuild {
    pub job: Job,
    pub recipes: BTreeMap<String, Recipe>,
}
#[derive(Debug, Clone)]
pub struct Evaluation {
    pub head: String,
    pub recipes: BTreeMap<String, Recipe>,
    pub at: u64,
    /// None means evaluation failed, not that there is nothing to build.
    pub outputs: Option<BTreeMap<String, Derivation>>,
    pub derivations_present: bool,
    pub merged: Option<(String, BTreeSet<String>)>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Derivation {
    pub path: String,
    pub outputs: BTreeSet<String>,
}
#[derive(Debug, Clone)]
pub struct ProofRecord {
    pub job: Job,
    pub recipes: BTreeMap<String, Recipe>,
    /// None is an infrastructure failure, even when the unit exited zero.
    pub verdicts: Option<BTreeMap<String, Verdict>>,
    pub note_needed: bool,
}
#[derive(Debug, Clone)]
pub struct ProofHolder {
    pub number: u64,
    pub job: Job,
    pub paths: Option<BTreeSet<String>>,
}
#[derive(Debug, Clone)]
pub struct ProofWaiting {
    pub number: u64,
    pub first: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProofPlan {
    Evaluate(BTreeMap<String, Recipe>),
    Build(BTreeMap<String, Derivation>),
    Run(BTreeMap<String, Recipe>),
    Job(BTreeMap<String, Recipe>),
    Prebuild(BTreeSet<String>),
    Stored(BTreeMap<String, Derivation>),
    Post {
        job: String,
        verdicts: BTreeMap<String, Verdict>,
        note: bool,
    },
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
fn selected(f: &Facts) -> BTreeMap<String, Recipe> {
    let pending: BTreeMap<_, _> = f
        .checks
        .iter()
        .filter(|c| c.state == Ci::Pending)
        .filter_map(|c| {
            f.settings
                .recipe(&c.name)
                .map(|r| (c.name.clone(), r.clone()))
        })
        .collect();
    for rank in 0..3 {
        let selected: BTreeMap<_, _> = pending
            .iter()
            .filter(|(_, recipe)| match recipe {
                Recipe::Build(_) => rank == 0,
                Recipe::Run(_) => rank == 1,
                Recipe::Job(_) => rank == 2,
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if !selected.is_empty() {
            return selected;
        }
    }
    pending
}
pub(super) fn engine_job(f: &Facts, id: &str) -> bool {
    let recipes = f
        .proof_data
        .records
        .iter()
        .find(|r| r.job.id == id)
        .map(|r| r.recipes.clone())
        .unwrap_or_else(|| selected(f));
    recipes.values().all(|r| matches!(r, Recipe::Job(_)))
}

fn evaluation<'a>(f: &'a Facts, recipes: &BTreeMap<String, Recipe>) -> Option<&'a Evaluation> {
    f.proof_data
        .evaluation
        .as_ref()
        .filter(|e| e.head == f.revision.head && &e.recipes == recipes)
}

/// A running old proof may be building an unchanged check on the new head.
pub(super) fn useful(f: &Facts, id: &str) -> bool {
    let selected = selected(f);
    if !selected.values().all(|r| matches!(r, Recipe::Build(_))) {
        return false;
    }
    let Some(outputs) = evaluation(f, &selected).and_then(|e| e.outputs.as_ref()) else {
        return false;
    };
    f.proof_data
        .live
        .iter()
        .filter(|h| h.job.id == id)
        .any(|h| {
            h.paths.as_ref().is_some_and(|paths| {
                outputs
                    .values()
                    .flat_map(|d| &d.outputs)
                    .any(|p| !f.proof_data.valid_paths.contains(p) && paths.contains(p))
            })
        })
}

/// Number ahead, or zero when paths are already being built. Unknown paths
/// hold a slot each. Only open PRs and earlier waiters in this pass count.
fn ahead(f: &Facts, outputs: Option<&BTreeSet<String>>) -> (u32, bool) {
    let Some(slots) = f.settings.proof_slots else {
        return (0, false);
    };
    let mut holders: Vec<_> = f
        .proof_data
        .live
        .iter()
        .filter(|h| f.open_prs.contains(&h.number) && h.job.state.live())
        .collect();
    holders.sort_by_key(|h| (h.job.started, &h.job.id));
    let mut building = BTreeSet::new();
    let mut count = 0;
    for h in holders {
        if let Some(paths) = &h.paths {
            let rest: BTreeSet<_> = paths
                .difference(&f.proof_data.valid_paths)
                .cloned()
                .collect();
            if !rest.is_subset(&building) {
                count += 1;
            }
            building.extend(rest);
        } else {
            count += 1;
        }
    }
    count += f
        .proof_data
        .waiting
        .iter()
        .filter(|w| (!w.first, w.number) < (!f.labels.contains(&f.settings.first_label), f.number))
        .count() as u32;
    if count < slots {
        return (0, false);
    }
    if outputs.is_some_and(|paths| {
        paths
            .difference(&f.proof_data.valid_paths)
            .all(|p| building.contains(p))
    }) {
        return (0, true);
    }
    (count, false)
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
        if recipes
            .values()
            .any(|r| matches!(r, Recipe::Build(_) | Recipe::Run(_)))
            && record.verdicts.is_none()
        {
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

pub(super) fn obsolete_inputs(f: &Facts) -> Option<&Job> {
    let inputs = f.proof_data.inputs_job.as_ref()?;
    let wanted = selected(f);
    (inputs.job.state.live()
        && (inputs.job.revision.head != f.revision.head
            || inputs.recipes != wanted
            || !wanted.values().all(|r| matches!(r, Recipe::Build(_)))
            || f.settings.proof_slots.is_none()))
    .then_some(&inputs.job)
}

pub(super) fn next(f: &Facts, now: u64) -> Option<(Stage, Step, String, Option<ProofPlan>)> {
    let recipes = selected(f);
    if recipes.is_empty() {
        return None;
    }
    let builds = recipes.values().all(|r| matches!(r, Recipe::Build(_)));
    let run = recipes.values().all(|r| matches!(r, Recipe::Run(_)));
    if !builds
        && !run
        && f.checks
            .iter()
            .any(|c| c.state == Ci::Pending && f.settings.recipe(&c.name).is_none())
    {
        return None;
    }
    let selected_names: BTreeSet<_> = recipes.keys().collect();
    for r in &f.proof_data.records {
        if !current(f, r) {
            continue;
        }
        let names: BTreeSet<_> = r.recipes.keys().collect();
        let fits = selected_names.is_subset(&names)
            && recipes.iter().all(|(k, v)| r.recipes.get(k) == Some(v));
        if r.job.state.live() {
            return Some(if fits {
                (
                    Stage::Proof,
                    Step::Wait {
                        until: Some({
                            let timeout = if builds || run {
                                f.settings.proof_timeout
                            } else {
                                f.settings.job_timeout
                            };
                            let deadline = r.job.started.saturating_add(timeout + 300);
                            if now < deadline {
                                deadline
                            } else {
                                now.saturating_add(900)
                            }
                        }),
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
    }
    let eval = builds.then(|| evaluation(f, &recipes)).flatten();
    let drvs = eval
        .filter(|e| e.derivations_present)
        .and_then(|e| e.outputs.as_ref());
    if builds && drvs.is_none() {
        if let Some(e) = eval.filter(|e| e.outputs.is_none() && now < e.at.saturating_add(900)) {
            return Some((
                Stage::Proof,
                Step::Wait {
                    until: Some(e.at.saturating_add(900)),
                },
                "Proof evaluation failed; retry after fifteen minutes".into(),
                None,
            ));
        }
        return Some((
            Stage::Proof,
            Step::Prove,
            "Evaluate the proof's exact derivations".into(),
            Some(ProofPlan::Evaluate(recipes)),
        ));
    }
    let paths: Option<BTreeSet<String>> = drvs.map(|ds| {
        ds.values()
            .flat_map(|d| d.outputs.iter().cloned())
            .collect()
    });
    if let (Some(ds), Some(paths)) = (drvs, &paths) {
        if !paths.is_empty() && paths.is_subset(&f.proof_data.valid_paths) {
            return Some((
                Stage::Proof,
                Step::PostProof,
                "Exact proof derivations already passed in this machine's store".into(),
                Some(ProofPlan::Stored(ds.clone())),
            ));
        }
        if f.dependency.is_none()
            && f.revision.base == f.default_branch
            && eval
                .and_then(|e| e.merged.as_ref())
                .is_some_and(|(tip, paths)| {
                    f.base_tip.as_ref() == Some(tip)
                        && !paths.is_empty()
                        && paths.is_subset(&f.proof_data.valid_paths)
                })
        {
            return Some((
                Stage::Proof,
                Step::UpdateBranch,
                "Bring the branch up to date; its merged proof is already built".into(),
                None,
            ));
        }
    }
    let (ahead, free) = ahead(f, paths.as_ref());
    if f.settings.proof_slots.is_some()
        && builds
        && !free
        && !f.proof_data.inputs.is_subset(&f.proof_data.valid_paths)
        && f.proof_data
            .inputs_job
            .as_ref()
            .is_none_or(|j| j.job.state.live())
    {
        if f.proof_data
            .inputs_job
            .as_ref()
            .is_some_and(|j| j.job.state.live())
        {
            return Some((
                Stage::ProofSlot,
                Step::Wait {
                    until: f.proof_data.inputs_job.as_ref().map(|j| {
                        let deadline = j.job.started.saturating_add(f.settings.proof_timeout + 300);
                        if now < deadline {
                            deadline
                        } else {
                            now.saturating_add(900)
                        }
                    }),
                },
                "Building the proof's inputs before its turn".into(),
                None,
            ));
        }
        return Some((
            Stage::ProofSlot,
            Step::Prove,
            "Build the proof's inputs before its turn".into(),
            Some(ProofPlan::Prebuild(f.proof_data.inputs.clone())),
        ));
    }
    if ahead > 0 {
        return Some((
            Stage::ProofSlot,
            Step::Wait { until: None },
            format!("Waiting for a machine proof slot ({ahead} ahead)"),
            None,
        ));
    }
    let plan = if let Some(ds) = drvs {
        ProofPlan::Build(ds.clone())
    } else if run {
        ProofPlan::Run(recipes)
    } else {
        ProofPlan::Job(recipes)
    };
    Some((
        Stage::Proof,
        Step::Prove,
        if free {
            "Start proof; nothing new to build, so it skipped the line"
        } else {
            "Start the next machine proof"
        }
        .into(),
        Some(plan),
    ))
}
