//! Conary's flow defaults name no machine and start no paid work (#378): a
//! machine's values come from its own configuration, and an unset one leaves
//! its step waiting.
use super::flow_case::Case;
use crate::flow::*;
use crate::sim::{REMI, STEWARD};
use serde_json::json;

#[test]
fn defaults_name_no_machine_and_start_no_paid_work() {
    // Every field is named, so a new setting can't join without a say here.
    let Settings {
        train,
        job_login,
        proof_slots,
        proof_timeout: _,
        job_timeout: _,
        judge_engines,
        reviewers: _,
        review_authors,
        verdict_authors,
        judge_key,
        major_key,
        bot_emails,
        renovate_email,
        renovate_login,
        batch_label: _,
        hold_label: _,
        first_label: _,
        owner_label,
        people,
        require_ci,
        proofs,
        settle_delay: _,
        handoff_after: _,
        session_handoff_after: _,
        rounds: _,
        stall_after: _,
        hold_stall_after: _,
        train_stall_after: _,
        renovate_merge_after: _,
        renovate_run: _,
        renovate_sweep,
    } = Settings::default();
    // Spenders: off or zero.
    assert!(!train);
    assert_eq!(proof_slots, Some(0));
    assert!(proofs.is_empty());
    assert!(!renovate_sweep);
    // Gates stay closed.
    assert!(require_ci);
    // No machine's accounts, people, keys, label or engines.
    assert_eq!(job_login, None);
    assert!(judge_engines.is_empty());
    assert!(review_authors.is_empty());
    assert!(verdict_authors.is_empty());
    assert!(judge_key.is_none() && major_key.is_none());
    assert!(bot_emails.is_empty());
    assert_eq!((renovate_email, renovate_login), (None, None));
    assert_eq!(owner_label, None);
    assert!(people.is_empty());
}

#[test]
fn zero_proof_slots_start_no_proof_of_any_recipe() {
    for recipe in [
        Recipe::Job("exercise".into()),
        Recipe::Run(vec!["true".into()]),
        Recipe::Build("checks.proof".into()),
    ] {
        let mut c = Case::new();
        c.settings.proofs.insert("proof".into(), recipe.clone());
        c.settings.proof_slots = Some(1);
        assert_eq!(c.view(800).step, Step::Prove, "{recipe:?}");
        c.settings.proof_slots = Settings::default().proof_slots;
        let next = c.view(800);
        assert_eq!(
            (next.stage, next.step),
            (Stage::ProofSlot, Step::Wait { until: None }),
            "{recipe:?}"
        );
    }
}

#[test]
fn an_unset_owner_label_leaves_the_owner_step_waiting_unlisted() {
    let mut c = Case::new();
    c.secret();
    c.judge("pass");
    assert_eq!(c.view(4000).step, Step::ListOwner);
    c.settings.owner_label = None;
    let next = c.view(4000);
    assert_eq!(
        (next.stage, next.step),
        (Stage::Owner, Step::Wait { until: None })
    );
    assert!(next.description.contains("no owner label"));
}

#[test]
fn no_review_author_queues_no_review() {
    let mut c = Case::new();
    c.hub.remove_flow_reviews(REMI, 3);
    assert_eq!(c.view(4000).step, Step::QueueReview("reviewer".into()));
    c.settings.review_authors.clear();
    let next = c.view(4000);
    assert_eq!(
        (next.stage, next.step),
        (Stage::Reviewer, Step::Wait { until: None })
    );
}

#[test]
fn the_train_status_counts_from_the_configured_steward_alone() {
    for author in [STEWARD, "other-steward[bot]"] {
        let mut c = Case::new();
        c.settings.review_authors = ["other-steward[bot]".into()].into();
        c.hub.put_statuses(
            &c.scope().head,
            json!([{"context":"conary/train","state":"failure","creator":{"login":author,"type":"Bot"}}]),
        );
        let counted = c
            .facts()
            .checks
            .iter()
            .any(|k| k.name == "conary/train" && k.state == Ci::Red);
        assert_eq!(counted, author == "other-steward[bot]", "{author}");
    }
}
