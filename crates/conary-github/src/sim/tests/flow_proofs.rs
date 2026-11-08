use super::flow_case::Case;
use crate::flow::*;
use crate::sim::REMI;
use std::collections::{BTreeMap, BTreeSet};

fn build() -> Case {
    let mut c = Case::new();
    c.settings
        .proofs
        .insert("vm".into(), Recipe::Build("checks.vm".into()));
    c.proof_data.number = 3;
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
    assert_eq!(c.view(800).step, Step::Wait { until: Some(11800) });
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
    ProofHolder {
        number: 9,
        open: true,
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
    c.proof_data.live[2].open = false;
    assert_eq!(c.view(800).step, Step::Prove);
    c.proof_data.waiting.push(ProofWaiting {
        number: 1,
        first: false,
    });
    assert_eq!(c.view(800).stage, Stage::ProofSlot);
    c.proof_data.first = true;
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
