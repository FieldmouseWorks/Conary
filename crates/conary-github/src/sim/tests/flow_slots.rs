use super::flow_case::Case;
use crate::flow::*;
use crate::sim::REMI;
use std::collections::{BTreeMap, BTreeSet};
fn proof() -> Case {
    let mut c = Case::new();
    c.settings.proof_slots = Some(1);
    c.settings
        .proofs
        .insert("proof".into(), Recipe::Job("exercise".into()));
    c
}
fn holder(c: &Case, id: &str, number: u64, state: JobState) -> ProofHolder {
    if number != 3 {
        c.hub.pr(REMI, number, &super::head(number));
    }
    ProofHolder {
        number,
        paths: None,
        job: Job {
            id: id.into(),
            revision: c.scope(),
            state,
            started: 100,
            repeated: false,
        },
    }
}
#[test]
fn slots_count_open_jobs_through_drafts_pushes_and_failed_stops() {
    // slots.py:36,42,46,51,56,68,83: jobs hold slots until ended, even on old heads.
    let mut c = proof();
    c.proof_data
        .live
        .push(holder(&c, "first", 1, JobState::Running));
    assert_eq!(c.view(800).stage, Stage::ProofSlot);
    c.hub
        .edit_pull(REMI, 1, |p| p["draft"] = serde_json::json!(true));
    assert_eq!(c.view(800).stage, Stage::ProofSlot);
    c.hub.push(REMI, 1, &super::head(91));
    assert_eq!(c.view(800).stage, Stage::ProofSlot);
    c.hub
        .edit_pull(REMI, 1, |p| p["state"] = serde_json::json!("closed"));
    assert_eq!(c.view(800).step, Step::Prove);
    c.hub
        .edit_pull(REMI, 1, |p| p["state"] = serde_json::json!("open"));
    c.proof_data.live[0].job.state = JobState::Done;
    assert_eq!(c.view(800).step, Step::Prove);
    c.settings.proof_slots = Some(2);
    c.proof_data.live[0].job.state = JobState::Unknown;
    let mut newer = holder(&c, "second", 1, JobState::Running);
    newer.job.revision.head = super::head(92);
    c.proof_data.live.push(newer);
    assert!(c.view(800).description.contains("2 ahead"));
    c.settings.proof_slots = None;
    assert_eq!(c.view(800).step, Step::Prove); // slots.py:140
}
#[test]
fn first_label_precedes_older_waiters_and_a_departed_waiter_holds_no_turn() {
    // slots.py:92,113,122: current-pass waiters only, priority then PR number.
    let mut c = proof();
    c.proof_data.waiting = vec![ProofWaiting {
        number: 1,
        first: false,
    }];
    assert_eq!(c.view(800).stage, Stage::ProofSlot);
    c.label(&["first"]);
    assert_eq!(c.view(800).step, Step::Prove);
    c.proof_data.waiting.push(ProofWaiting {
        number: 2,
        first: true,
    });
    assert_eq!(c.view(800).stage, Stage::ProofSlot);
    c.proof_data.waiting.clear();
    assert_eq!(c.view(800).step, Step::Prove);
}
#[test]
fn a_new_proof_kind_does_not_inherit_an_ended_builds_lost_answer() {
    // proving.py:440 / runs.py:182: only matching recipes belong to a retry.
    for recipe in [
        Recipe::Run(vec!["check".into()]),
        Recipe::Job("check".into()),
    ] {
        let mut c = proof();
        c.settings.proofs.insert("proof".into(), recipe);
        c.proof_data.records.push(ProofRecord {
            job: holder(&c, "old-build", 3, JobState::Done).job,
            recipes: BTreeMap::from([("vm".into(), Recipe::Build("checks.vm".into()))]),
            verdicts: None,
            note_needed: false,
        });
        assert_eq!(c.pass(800).step, Step::Prove);
        assert!(!c.memory.strikes.contains_key(&Failure::Lost));
        assert!(c.memory.unanswered.is_empty());
    }
}
#[test]
fn pending_verdicts_are_bound_to_their_kind_command_and_attribute() {
    // runs.py:216 / proving.py:453,487,507: remapped/dropped/kind-changed checks never receive old verdicts.
    for replacement in [
        None,
        Some(Recipe::Run(vec!["other".into()])),
        Some(Recipe::Build("same".into())),
        Some(Recipe::Job("same".into())),
    ] {
        let mut c = proof();
        c.settings.proofs = BTreeMap::from([
            ("gate".into(), Recipe::Run(vec!["same".into()])),
            ("keep".into(), Recipe::Run(vec!["keep".into()])),
        ]);
        let recipes = c.settings.proofs.clone();
        c.proof_data.records.push(ProofRecord {
            job: holder(&c, "result", 3, JobState::Done).job,
            recipes,
            verdicts: Some(BTreeMap::from([
                ("gate".into(), Verdict::Clean),
                ("keep".into(), Verdict::Findings),
            ])),
            note_needed: false,
        });
        match replacement {
            Some(recipe) => {
                c.settings.proofs.insert("gate".into(), recipe);
            }
            None => {
                c.settings.proofs.remove("gate");
            }
        }
        let n = c.view(800);
        assert!(
            matches!(n.proof,Some(ProofPlan::Post{ref verdicts,..}) if verdicts.keys().cloned().collect::<BTreeSet<_>>()==BTreeSet::from(["keep".into()]))
        );
    }
}
