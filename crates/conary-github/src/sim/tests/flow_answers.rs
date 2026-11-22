use super::flow_case::Case;
use crate::flow::*;

#[test]
fn effect_success_clears_only_its_own_failures_and_never_a_same_pass_failure() {
    // strikes.py:38 / 210 / 217: consecutive failures end at observed success; one failure per pass.
    let mut c = Case::new();
    c.failed = Some(Failure::Merge);
    c.pass(700);
    c.failed = Some(Failure::Status);
    c.pass(760);
    c.succeeded.insert(Failure::Merge);
    c.succeeded.insert(Failure::Status);
    c.pass(820);
    assert!(!c.memory.strikes.contains_key(&Failure::Merge));
    assert_eq!(c.memory.strikes[&Failure::Status], 2);
    c.failed = None;
    assert_eq!(c.pass(900).step, Step::Merge);
    assert!(c.memory.strikes.is_empty());
}
#[test]
fn repo_controller_holds_apply_before_reviews_proofs_or_landing() {
    // alone.py:63 / undos.py:121: repo controller holds its own red fix/undo while trunk's CI runs.
    // Identifying that undo belongs to the controller is persisted repo state,
    // supplied by that controller; no cn writer or default-branch controller is run here.
    for branch in ["autoland/red-cccccccccccc", "conary/undo/c40/1"] {
        let mut c = Case::new();
        c.hub.edit_pull(crate::sim::REMI, 3, |p| {
            p["head"]["ref"] = serde_json::json!(branch)
        });
        c.hub.refresh_flow(crate::sim::REMI, 3);
        c.hub.ci(super::TRUNK, &[("main-ci", "failure")]);
        c.base_held = true;
        c.settings
            .proofs
            .insert("proof".into(), Recipe::Run(vec!["prove it".into()]));
        c.programs();
        assert_eq!(c.view(700).stage, Stage::Base);
        assert!(matches!(c.view(700).step, Step::Wait { .. }));
        c.base_held = false;
        assert_eq!(c.view(800).step, Step::Prove);
    }
}

#[test]
fn failed_status_revocation_is_retried_even_while_held_or_draft() {
    // passes.py:29 / 51: an old success/auto-merge must be revoked even while set aside.
    for draft in [false, true] {
        let mut c = Case::new();
        c.label(&["hold"]);
        if draft {
            c.hub.edit_flow(crate::sim::REMI, 3, |s| s.draft = true);
        }
        c.failed = Some(Failure::Status);
        assert_eq!(
            c.pass(500).step,
            Step::Retry {
                kind: Failure::Status,
                at: 560
            }
        );
        assert_eq!(
            c.pass(600).step,
            Step::Retry {
                kind: Failure::Status,
                at: 720
            }
        );
        c.failed = None;
        c.succeeded.insert(Failure::Status);
        assert!(matches!(c.pass(700).step, Step::Wait { .. }));
        assert!(c.memory.strikes.is_empty());
    }
}

/// A failed effect's own words go into the status of its retry and of the
/// wait before it, never dropped (#551).
#[test]
fn a_failed_effect_says_why_while_it_waits_to_try_again() {
    let mut c = Case::new();
    c.failed = Some(Failure::Job);
    c.failed_why = Some("the job could not start".into());
    let failed = c.pass(700);
    assert!(matches!(failed.step, Step::Retry { .. }));
    assert_eq!(
        failed.description,
        "The step failed; trying again later: the job could not start"
    );
    c.failed = None;
    c.failed_why = None;
    assert_eq!(
        c.pass(701).description,
        "Waiting to try the failed step again: the job could not start"
    );
}
