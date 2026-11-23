use super::flow_case::Case;
use crate::flow::*;
use crate::sim::REMI;
use std::collections::BTreeMap;

fn build() -> Case {
    let mut c = Case::new();
    c.settings
        .proofs
        .insert("vm".into(), Recipe::Build("checks.vm".into()));
    c
}
fn ready(slot: bool) -> Readiness {
    Readiness {
        ready: true,
        slot,
        why: "inputs built".into(),
    }
}
fn record(c: &Case, state: JobState) -> ProofRecord {
    ProofRecord {
        job: Job {
            id: "proof-1".into(),
            revision: c.scope(),
            state,
            started: 700,
            repeated: false,
        },
        recipes: c.settings.proofs.clone(),
        verdicts: None,
        note_needed: false,
        sized: crate::flow::durations::Sized::configured(10_800),
    }
}
#[test]
fn partially_posted_failure_flushes_other_verdicts_before_handoff() {
    // proving.py: a posted failure must not strand its sibling's verdict behind red CI.
    let mut c = build();
    c.settings
        .proofs
        .insert("other".into(), Recipe::Build("checks.other".into()));
    let mut r = record(&c, JobState::Done);
    r.verdicts = Some(BTreeMap::from([
        ("vm".into(), Verdict::Findings),
        ("other".into(), Verdict::Clean),
    ]));
    r.note_needed = true;
    c.proof_data.records.push(r);
    c.hub.put_statuses(
        &c.scope().head,
        serde_json::json!([{"context":"vm","state":"failure"}]),
    );
    assert_eq!(c.view(8000).step, Step::PostProof);
    assert_eq!(
        c.view(8000).proof,
        Some(ProofPlan::Post {
            job: "proof-1".into(),
            verdicts: BTreeMap::from([("other".into(), Verdict::Clean)]),
            note: true
        })
    );
    c.settings
        .proofs
        .insert("other".into(), Recipe::Build("checks.remapped".into()));
    assert_eq!(c.view(8000).step, Step::HandFix(Kind::Red));
}
#[test]
fn a_running_job_with_one_check_already_posted_is_the_same_proof() {
    // proving.py: a proof which posted one of two checks keeps running the other.
    let mut c = Case::new();
    c.settings.proofs = BTreeMap::from([
        ("a".into(), Recipe::Run(vec!["run a".into()])),
        ("b".into(), Recipe::Run(vec!["run b".into()])),
    ]);
    c.proof_data.records.push(record(&c, JobState::Running));
    c.hub.put_statuses(
        &c.scope().head,
        serde_json::json!([{"context":"a","state":"success"}]),
    );
    assert_eq!(c.view(800).step, Step::Wait { until: Some(11800) });
    c.settings
        .proofs
        .insert("b".into(), Recipe::Build("checks.b".into()));
    assert_eq!(c.view(900).step, Step::StopJob("proof-1".into()));
}
fn holder(c: &Case, id: &str, at: u64) -> ProofHolder {
    let number = match id {
        "a" => 7,
        "b" => 8,
        "Proof-1" => 3,
        _ => 9,
    };
    if number != 3 {
        c.hub.pr(REMI, number, &super::head(number));
    }
    ProofHolder {
        number,
        job: Job {
            id: id.into(),
            revision: c.scope(),
            state: JobState::Running,
            started: at,
            repeated: false,
        },
    }
}

#[test]
fn proof_slots_count_each_holder_and_only_current_waiters() {
    // proving.py: each running proof takes one slot.
    let mut c = build();
    c.ready(true);
    c.settings.proof_slots = Some(2);
    c.proof_data.live = vec![holder(&c, "a", 100)];
    assert_eq!(c.view(800).step, Step::Prove);
    c.proof_data.live.push(holder(&c, "unknown", 300));
    assert_eq!(c.view(800).stage, Stage::ProofSlot);
    c.hub
        .edit_pull(REMI, 9, |p| p["state"] = serde_json::json!("closed"));
    assert_eq!(c.view(800).step, Step::Prove);
    c.hub.pr(REMI, 1, &super::head(1));
    c.hub.edit_pull(REMI, 1, |p| {
        p["created_at"] = serde_json::json!("2026-01-01T00:00:00Z");
    });
    c.proof_data.waiting.push(ProofWaiting { number: 1 });
    assert_eq!(c.view(800).stage, Stage::ProofSlot);
    c.label(&["priority: high"]);
    assert_eq!(c.view(800).step, Step::Prove);
    c.hub.label(REMI, 1, &["priority: high"]);
    c.proof_data.waiting.push(ProofWaiting { number: 1 });
    assert_eq!(c.view(800).stage, Stage::ProofSlot);
}
#[test]
fn an_ended_machine_proof_without_verdicts_is_retried_as_infrastructure_failure() {
    // proving.py:378 / 392: a done unit with no verdicts broke; never fail the PR for it.
    for state in [JobState::Done, JobState::Broken] {
        let mut c = build();
        c.ready(true);
        c.proof_data.records.push(record(&c, state));
        assert_eq!(c.pass(900).step, Step::Wait { until: Some(960) });
        assert_eq!(
            c.pass(960).step,
            Step::Retry {
                kind: Failure::Lost,
                at: 1020
            }
        );
        assert_eq!(c.pass(1100).step, Step::Prove);
        assert_eq!(c.memory.strikes[&Failure::Lost], 1);
        assert!(matches!(
            c.view(1100).proof,
            Some(ProofPlan::Prove { slot: true, .. })
        ));
    }
}

#[test]
fn proof_context_case_changes_keep_configured_names_and_only_unposted_work() {
    // builds.py:330,350,354,362,379,398,404,414,420,425.
    let mut c = build();
    c.settings.proofs = BTreeMap::from([
        ("VM".into(), Recipe::Build("checks.vm".into())),
        ("Other".into(), Recipe::Build("checks.other".into())),
    ]);
    let mut r = record(&c, JobState::Done);
    r.verdicts = Some(BTreeMap::from([
        ("VM".into(), Verdict::Clean),
        ("Other".into(), Verdict::Clean),
    ]));
    c.proof_data.records.push(r);
    for name in ["vm", "Vm", "VM"] {
        c.hub.put_statuses(
            &c.scope().head,
            serde_json::json!([
                {"context":name,"state":"success"}, {"context":"oTHER","state":"pending"}
            ]),
        );
        assert_eq!(
            c.view(800).proof,
            Some(ProofPlan::Post {
                job: "proof-1".into(),
                verdicts: BTreeMap::from([("Other".into(), Verdict::Clean)]),
                note: false,
            })
        );
    }
    c.hub.put_statuses(
        &c.scope().head,
        serde_json::json!([
            {"context":"vM","state":"success"}, {"context":"OTHER","state":"success"}
        ]),
    );
    assert_eq!(c.view(800).step, Step::Merge);
    c.hub.put_statuses(
        &c.scope().head,
        serde_json::json!([
            {"context":"vM","state":"failure"}, {"context":"OTHER","state":"success"}
        ]),
    );
    assert_eq!(c.view(8000).step, Step::HandFix(Kind::Red));
    c.proof_data.records.clear();
    c.hub.put_statuses(
        &c.scope().head,
        serde_json::json!([
            {"context":"vM","state":"pending"}, {"context":"OTHER","state":"success"}
        ]),
    );
    assert_eq!(
        c.view(8000).proof,
        Some(ProofPlan::Prepare(BTreeMap::from([(
            "VM".into(),
            Recipe::Build("checks.vm".into())
        )])))
    );
}

#[test]
fn every_obsolete_live_proof_is_stopped_even_behind_newer_records() {
    for retired in [false, true] {
        let mut c = build();
        let mut a = record(&c, JobState::Running);
        a.job.revision.head = "old-head".into();
        let mut b = a.clone();
        b.job.id = "proof-2".into();
        b.job.started += 1;
        if retired {
            c.memory.retired_jobs.insert(b.job.id.clone());
        } else {
            b.job.state = JobState::Done;
        }
        c.proof_data.records = vec![a, b];
        assert_eq!(c.pass(8000).step, Step::StopJob("proof-1".into()));
        assert_eq!(c.pass(8100).step, Step::StopJob("proof-1".into()));
        c.stopped_jobs.insert("proof-1".into());
        assert!(!matches!(c.pass(8200).step, Step::StopJob(_)));
    }
}

#[test]
fn recipe_changes_cancel_every_live_record_before_new_work() {
    let mut c = build();
    let a = record(&c, JobState::Running);
    let mut b = a.clone();
    b.job.id = "proof-2".into();
    b.job.started += 1;
    c.proof_data.records = vec![a, b];
    c.settings
        .proofs
        .insert("vm".into(), Recipe::Run(vec!["new-command".into()]));
    assert_eq!(c.pass(8000).step, Step::StopJob("proof-1".into()));
    c.stopped_jobs.insert("proof-1".into());
    assert_eq!(c.pass(8100).step, Step::StopJob("proof-2".into()));
    c.stopped_jobs.insert("proof-2".into());
    assert!(!matches!(c.pass(8200).step, Step::StopJob(_)));
}

#[test]
fn a_proof_is_prepared_first_and_a_failed_preparation_is_asked_again_after_fifteen_minutes() {
    let mut c = build();
    let recipes = c.settings.proofs.clone();
    let first = c.view(700);
    assert_eq!(first.step, Step::Prepare);
    assert_eq!(first.stage, Stage::Proof);
    assert_eq!(first.proof, Some(ProofPlan::Prepare(recipes.clone())));
    // Running: one at a time for this head and recipe set.
    c.prepared(None, None);
    assert_eq!(c.view(700).step, Step::Wait { until: Some(11700) });
    // Failed, or not ready: wait fifteen minutes, then ask again.
    for answer in [
        None,
        Some(Readiness {
            ready: false,
            slot: true,
            why: "the cache is cold".into(),
        }),
    ] {
        c.proof_data.prepared.clear();
        c.prepared(answer.clone(), Some(800));
        let waiting = c.view(900);
        assert_eq!(waiting.step, Step::Wait { until: Some(1700) });
        assert_eq!(waiting.proof, None);
        assert_eq!(
            answer.is_some(),
            waiting.description.contains("the cache is cold")
        );
        assert_eq!(c.view(1700).step, Step::Prepare);
    }
    // Ready: the proof starts, holding a slot only when it needs one.
    for slot in [true, false] {
        c.proof_data.prepared.clear();
        c.prepared(Some(ready(slot)), Some(800));
        let next = c.view(900);
        assert_eq!(next.step, Step::Prove);
        assert_eq!(
            next.proof,
            Some(ProofPlan::Prove {
                recipes: recipes.clone(),
                slot
            })
        );
    }
}

#[test]
fn a_remapped_recipe_or_another_head_needs_its_own_preparation() {
    let mut c = build();
    c.ready(true);
    assert_eq!(c.view(700).step, Step::Prove);
    c.settings
        .proofs
        .insert("vm".into(), Recipe::Build("checks.other".into()));
    assert!(matches!(c.view(800).proof, Some(ProofPlan::Prepare(_))));
    c.proof_data.prepared.clear();
    c.prepared(Some(ready(true)), Some(800));
    assert_eq!(c.view(900).step, Step::Prove);
    c.push("new-proof-head");
    c.review("reviewer", Verdict::Clean, 900);
    assert!(matches!(c.view(1300).proof, Some(ProofPlan::Prepare(_))));
}

#[test]
fn proofs_run_builds_then_commands_and_a_model_run_check_waits_for_neither() {
    // proving.py: commands start only after builds. A job is no proof.
    let mut c = build();
    c.settings
        .proofs
        .insert("run".into(), Recipe::Run(vec!["just proof".into()]));
    c.settings
        .proofs
        .insert("job".into(), Recipe::Job("exercise the change".into()));
    assert!(
        matches!(c.view(700).proof, Some(ProofPlan::Prepare(ref p)) if p.len()==1 && p.contains_key("vm"))
    );
    c.hub.put_statuses(
        &c.scope().head,
        serde_json::json!([{"context":"vm","state":"success"}]),
    );
    assert!(
        matches!(c.view(800).proof, Some(ProofPlan::Prepare(ref p)) if p.len()==1 && p.contains_key("run"))
    );
    c.hub.put_statuses(
        &c.scope().head,
        serde_json::json!([{"context":"vm","state":"success"},{"context":"run","state":"success"}]),
    );
    let next = c.view(900);
    assert_eq!(next.step, Step::Wait { until: None });
    assert_eq!(next.proof, None);
    assert!(
        next.description
            .contains("job is a model-run check, not a proof"),
        "{}",
        next.description
    );
}

#[test]
fn a_model_run_check_is_never_prepared_or_proved_even_after_an_earlier_proof() {
    let mut c = Case::new();
    c.settings.proofs = BTreeMap::from([("job".into(), Recipe::Job("exercise it".into()))]);
    for now in [700, 5000, 90000] {
        let next = c.pass(now);
        assert_eq!(next.step, Step::Wait { until: None });
        assert!(next.description.contains("model-run"));
    }
    // An earlier proof that ended on this head isn't retried for it.
    c.proof_data.records.push(record(&c, JobState::Done));
    for now in [100000, 100060, 200000] {
        assert_eq!(c.pass(now).step, Step::Wait { until: None });
    }
    assert!(c.memory.strikes.is_empty());
}

#[test]
fn a_preparation_runs_outside_the_line_and_one_needing_no_slot_skips_it() {
    let mut c = build();
    c.settings.proof_slots = Some(1);
    c.proof_data.live = vec![holder(&c, "other", 100)];
    assert_eq!(c.view(800).step, Step::Prepare);
    assert_eq!(c.view(800).stage, Stage::Proof);
    c.prepared(Some(ready(true)), Some(700));
    assert_eq!(c.view(800).stage, Stage::ProofSlot);
    assert_eq!(c.view(800).step, Step::Wait { until: None });
    c.proof_data.prepared.clear();
    c.prepared(Some(ready(false)), Some(700));
    let next = c.view(800);
    assert_eq!(next.step, Step::Prove);
    assert!(matches!(
        next.proof,
        Some(ProofPlan::Prove { slot: false, .. })
    ));
}

#[test]
fn zero_slots_start_no_preparation_either() {
    let mut c = build();
    c.settings.proof_slots = Some(0);
    assert_eq!(c.view(700).stage, Stage::ProofSlot);
    assert_eq!(c.view(700).step, Step::Wait { until: None });
    c.prepared(Some(ready(false)), Some(650));
    assert_eq!(c.view(700).step, Step::Wait { until: None });
}

#[test]
fn obsolete_preparations_are_stopped_across_pushes_remaps_and_model_run_recipes() {
    for change in 0..3 {
        let mut c = build();
        c.prepared(None, None);
        match change {
            0 => {
                c.push("new-input-head");
                c.review("reviewer", Verdict::Clean, 800);
            }
            1 => {
                c.settings
                    .proofs
                    .insert("vm".into(), Recipe::Build("checks.other".into()));
            }
            _ => {
                c.settings
                    .proofs
                    .insert("vm".into(), Recipe::Job("exercise it".into()));
            }
        }
        assert_eq!(c.pass(1300).step, Step::StopJob("prepare-1".into()));
        assert_eq!(c.pass(1400).step, Step::StopJob("prepare-1".into()));
        c.failed = Some(Failure::Stop);
        assert_eq!(
            c.pass(1500).step,
            Step::Retry {
                kind: Failure::Stop,
                at: 1560
            }
        );
        c.failed = None;
        c.stopped_jobs.insert("prepare-1".into());
        assert_ne!(c.pass(1600).step, Step::StopJob("prepare-1".into()));
        assert!(c.memory.stopping.is_empty());
    }
}
