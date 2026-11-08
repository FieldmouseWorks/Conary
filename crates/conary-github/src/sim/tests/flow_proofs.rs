use super::flow_case::Case;
use crate::flow::*;
use crate::sim::REMI;
use std::collections::{BTreeMap, BTreeSet};

fn build() -> Case {
    let mut c = Case::new();
    c.settings
        .proofs
        .insert("vm".into(), Recipe::Build("checks.vm".into()));
    c
}
fn drv() -> Derivation {
    Derivation {
        path: "/nix/store/test.drv".into(),
        outputs: BTreeSet::from(["/nix/store/test".into()]),
    }
}
fn evaluate(c: &mut Case) {
    c.proof_data.evaluation = Some(Evaluation {
        head: c.scope().head,
        recipes: c.settings.proofs.clone(),
        at: 700,
        outputs: Some(BTreeMap::from([("vm".into(), drv())])),
        derivations_present: true,
        merged: None,
    });
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
    }
}
#[test]
fn builds_evaluate_before_start_and_retry_failed_evaluation_after_fifteen_minutes() {
    // proving.py / builds.py: no build without exact derivations; failed eval is retried.
    let mut c = build();
    assert!(matches!(c.view(700).proof, Some(ProofPlan::Evaluate(_))));
    evaluate(&mut c);
    c.proof_data.evaluation.as_mut().unwrap().outputs = None;
    assert_eq!(c.view(800).step, Step::Wait { until: Some(1600) });
    assert!(matches!(c.view(1600).proof, Some(ProofPlan::Evaluate(_))));
    evaluate(&mut c);
    assert_eq!(
        c.view(1700).proof,
        Some(ProofPlan::Build(BTreeMap::from([("vm".into(), drv())])))
    );
}
#[test]
fn exact_store_outputs_post_a_pass_without_a_job_or_slot() {
    // builds.py: outputs already built are evidence for the exact derivation, no rerun.
    let mut c = build();
    evaluate(&mut c);
    c.settings.proof_slots = Some(1);
    c.proof_data.valid_paths = drv().outputs;
    assert_eq!(c.view(700).step, Step::PostProof);
    assert!(matches!(c.view(700).proof, Some(ProofPlan::Stored(_))));
    c.proof_data
        .evaluation
        .as_mut()
        .unwrap()
        .derivations_present = false;
    assert!(matches!(c.view(800).proof, Some(ProofPlan::Evaluate(_))));
}
#[test]
fn proof_recipe_remapping_or_another_head_invalidates_cached_evaluation() {
    // proving.py: a check added or remapped is a different proof.
    let mut c = build();
    evaluate(&mut c);
    c.settings
        .proofs
        .insert("vm".into(), Recipe::Build("checks.other".into()));
    assert!(matches!(c.view(800).proof, Some(ProofPlan::Evaluate(_))));
    evaluate(&mut c);
    c.push("new-proof-head");
    c.review("reviewer", Verdict::Clean, 900);
    assert!(matches!(c.view(1300).proof, Some(ProofPlan::Evaluate(_))));
}
#[test]
fn proofs_prioritize_builds_then_commands_then_jobs() {
    // proving.py: commands and jobs start only after builds, jobs after commands.
    let mut c = build();
    c.settings
        .proofs
        .insert("run".into(), Recipe::Run(vec!["just proof".into()]));
    c.settings
        .proofs
        .insert("job".into(), Recipe::Job("exercise the change".into()));
    let plan = c.view(700).proof.unwrap();
    assert!(matches!(plan, ProofPlan::Evaluate(ref p) if p.len()==1 && p.contains_key("vm")));
    c.hub.put_statuses(
        &c.scope().head,
        serde_json::json!([{"context":"vm","state":"success"}]),
    );
    assert!(
        matches!(c.view(800).proof, Some(ProofPlan::Run(ref p)) if p.len()==1 && p.contains_key("run"))
    );
    c.hub.put_statuses(
        &c.scope().head,
        serde_json::json!([{"context":"vm","state":"success"},{"context":"run","state":"success"}]),
    );
    assert!(
        matches!(c.view(900).proof, Some(ProofPlan::Job(ref p)) if p.len()==1 && p.contains_key("job"))
    );
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
    // proving.py: a job which posted one of two checks keeps running the other.
    let mut c = Case::new();
    c.settings.proofs = BTreeMap::from([
        ("a".into(), Recipe::Job("run a".into())),
        ("b".into(), Recipe::Job("run b".into())),
    ]);
    c.proof_data.records.push(record(&c, JobState::Running));
    c.hub.put_statuses(
        &c.scope().head,
        serde_json::json!([{"context":"a","state":"success"}]),
    );
    assert_eq!(c.view(800).step, Step::Wait { until: Some(4600) });
    c.settings
        .proofs
        .insert("b".into(), Recipe::Build("checks.b".into()));
    assert_eq!(c.view(900).step, Step::StopJob("proof-1".into()));
}
#[test]
fn already_built_merged_outputs_update_only_the_exact_default_tip() {
    // builds.py: an ordinary PR may update instead of rebuilding what trunk already built.
    let mut c = build();
    evaluate(&mut c);
    let tip = c.facts().base_tip.unwrap();
    let merged = BTreeSet::from(["/nix/store/trunk".into()]);
    c.proof_data.valid_paths = merged.clone();
    c.proof_data.evaluation.as_mut().unwrap().merged = Some((tip, merged));
    assert_eq!(c.view(800).step, Step::UpdateBranch);
    c.hub.edit_pull(REMI, 3, |p| {
        p["head"]["ref"] = serde_json::json!("dependabot/pkg")
    });
    c.hub.refresh_flow(REMI, 3);
    assert_ne!(c.view(900).step, Step::UpdateBranch);
}

fn holder(c: &Case, id: &str, paths: Option<BTreeSet<String>>, at: u64) -> ProofHolder {
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
        paths,
    }
}

#[test]
fn proof_slots_count_unique_builds_unknown_jobs_and_only_current_waiters() {
    // builds.py / proving.py: shared paths take one slot; unknown jobs each take one.
    let mut c = build();
    evaluate(&mut c);
    c.settings.proof_slots = Some(2);
    let shared = BTreeSet::from(["/nix/store/shared".into()]);
    c.proof_data.live = vec![
        holder(&c, "a", Some(shared.clone()), 100),
        holder(&c, "b", Some(shared), 200),
    ];
    assert_eq!(c.view(800).step, Step::Prove);
    c.proof_data.live.push(holder(&c, "unknown", None, 300));
    assert_eq!(c.view(800).stage, Stage::ProofSlot);
    c.hub
        .edit_pull(REMI, 9, |p| p["state"] = serde_json::json!("closed"));
    assert_eq!(c.view(800).step, Step::Prove);
    c.proof_data.waiting.push(ProofWaiting {
        number: 1,
        first: false,
    });
    assert_eq!(c.view(800).stage, Stage::ProofSlot);
    c.label(&["first"]);
    assert_eq!(c.view(800).step, Step::Prove);
    c.proof_data.waiting.push(ProofWaiting {
        number: 1,
        first: true,
    });
    assert_eq!(c.view(800).stage, Stage::ProofSlot);
}
#[test]
fn shared_outputs_skip_the_line_and_prebuilds_run_outside_it() {
    // builds.py: already building outputs skip the line; inputs are built before a waiting turn.
    let mut c = build();
    evaluate(&mut c);
    c.settings.proof_slots = Some(1);
    c.proof_data
        .live
        .push(holder(&c, "shared", Some(drv().outputs), 100));
    assert_eq!(c.view(800).step, Step::Prove);
    assert!(c.view(800).description.contains("skipped the line"));
    c.proof_data.live[0].paths = None;
    c.proof_data.inputs.insert("/nix/store/input".into());
    assert_eq!(
        c.view(800).proof,
        Some(ProofPlan::Prebuild(c.proof_data.inputs.clone()))
    );
    assert_eq!(c.view(800).stage, Stage::ProofSlot);
    c.proof_data.valid_paths = c.proof_data.inputs.clone();
    assert_eq!(c.view(800).step, Step::Wait { until: None });
}
#[test]
fn an_old_proof_building_the_new_heads_outputs_is_left_running() {
    // superseded.py / builds.py: unchanged outputs on a new head reuse work, not its verdict.
    let mut c = build();
    let old = record(&c, JobState::Running);
    c.jobs.insert(Kind::Proof, old.job.clone());
    c.proof_data
        .live
        .push(holder(&c, &old.job.id, Some(drv().outputs), 700));
    c.push("head-with-unchanged-check");
    c.review("reviewer", Verdict::Clean, 800);
    evaluate(&mut c);
    assert_ne!(c.view(1200).step, Step::StopJob(old.job.id.clone()));
    c.proof_data.evaluation.as_mut().unwrap().outputs = None;
    assert_eq!(c.view(1200).step, Step::StopJob(old.job.id));
}

#[test]
fn an_ended_machine_proof_without_verdicts_is_retried_as_infrastructure_failure() {
    // proving.py:378 / 392: a done unit with no verdicts broke; never fail the PR for it.
    for state in [JobState::Done, JobState::Broken] {
        let mut c = build();
        evaluate(&mut c);
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
        assert!(matches!(c.view(1100).proof, Some(ProofPlan::Build(_))));
    }
}

fn inputs(c: &Case, state: JobState) -> InputBuild {
    InputBuild {
        job: Job {
            id: "inputs".into(),
            state,
            ..record(c, state).job
        },
        recipes: c.settings.proofs.clone(),
    }
}
#[test]
fn inputs_run_outside_the_line_and_the_first_finished_takes_the_slot() {
    // builds.py:552,581,587,590,598,604,615,629.
    let mut c = build();
    evaluate(&mut c);
    c.settings.proof_slots = Some(1);
    c.proof_data.inputs.insert("/nix/store/input".into());
    assert!(matches!(c.view(700).proof, Some(ProofPlan::Prebuild(_))));
    c.proof_data.inputs_job = Some(inputs(&c, JobState::Running));
    assert_eq!(c.view(800).step, Step::Wait { until: Some(11800) });
    assert_eq!(c.view(11800).step, Step::Wait { until: Some(12700) });
    for state in [JobState::Done, JobState::Broken] {
        c.proof_data.inputs_job = Some(inputs(&c, state));
        assert!(matches!(c.view(900).proof, Some(ProofPlan::Build(_))));
        c.proof_data.live = vec![holder(&c, "other", None, 800)];
        assert_eq!(c.view(1000).stage, Stage::ProofSlot);
        c.proof_data.live.clear();
    }
    c.proof_data.inputs_job = None;
    c.proof_data.valid_paths = c.proof_data.inputs.clone();
    assert!(matches!(c.view(1000).proof, Some(ProofPlan::Build(_))));
    c.proof_data.valid_paths.clear();
    c.settings.proof_slots = None;
    assert!(matches!(c.view(1000).proof, Some(ProofPlan::Build(_))));
}
#[test]
fn obsolete_prebuilds_are_stopped_across_pushes_remaps_and_recipe_kinds() {
    // builds.py:636,649,668: obsolete input work is stopped, even if new outputs are built.
    for change in 0..4 {
        let mut c = build();
        evaluate(&mut c);
        c.settings.proof_slots = Some(1);
        c.proof_data.inputs.insert("/nix/store/input".into());
        c.proof_data.inputs_job = Some(inputs(&c, JobState::Running));
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
            2 => {
                c.settings
                    .proofs
                    .insert("vm".into(), Recipe::Job("exercise it".into()));
            }
            _ => {
                c.settings.proof_slots = None;
            }
        }
        assert_eq!(c.pass(1300).step, Step::StopJob("inputs".into()));
        assert_eq!(c.pass(1400).step, Step::StopJob("inputs".into()));
        c.failed = Some(Failure::Stop);
        assert_eq!(
            c.pass(1500).step,
            Step::Retry {
                kind: Failure::Stop,
                at: 1560
            }
        );
        c.failed = None;
        c.stopped_jobs.insert("inputs".into());
        assert_ne!(c.pass(1600).step, Step::StopJob("inputs".into()));
        assert!(c.memory.stopping.is_empty());
    }
}
#[test]
fn run_and_job_recipes_never_consume_build_evaluations_or_store_outputs() {
    // runs.py:182,227: each kind starts afresh, and remapped commands are different proofs.
    for recipe in [
        Recipe::Run(vec!["check".into()]),
        Recipe::Job("check".into()),
    ] {
        let mut c = build();
        c.settings.proofs.insert("vm".into(), recipe.clone());
        evaluate(&mut c);
        c.proof_data.valid_paths = drv().outputs;
        let plan = c.view(800).proof.unwrap();
        assert_eq!(
            plan,
            match recipe {
                Recipe::Run(_) => ProofPlan::Run(c.settings.proofs.clone()),
                _ => ProofPlan::Job(c.settings.proofs.clone()),
            }
        );
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
        Some(ProofPlan::Evaluate(BTreeMap::from([(
            "VM".into(),
            Recipe::Build("checks.vm".into())
        )])))
    );
}

#[test]
fn one_of_two_outputs_is_never_a_stored_pass() {
    // builds.py:438: one output of two is insufficient.
    let mut c = build();
    evaluate(&mut c);
    c.proof_data.valid_paths = drv().outputs;
    c.proof_data
        .evaluation
        .as_mut()
        .unwrap()
        .outputs
        .as_mut()
        .unwrap()
        .get_mut("vm")
        .unwrap()
        .outputs
        .insert("/nix/store/second".into());
    assert!(matches!(c.view(800).proof, Some(ProofPlan::Build(_))));
}
