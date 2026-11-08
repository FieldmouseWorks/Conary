use super::flow_case::Case;
use crate::flow::*;

fn answered() -> Case {
    let mut c = Case::new();
    c.roles(&["reviewer"]);
    c.service("reviewer").answer = Some(Review {
        revision: c.scope(),
        verdict: Verdict::Findings,
        at: 600,
    });
    c
}
#[test]
fn stale_answers_are_consumed_before_a_new_look_including_after_hold() {
    // handle.py:82: an old answer cannot re-raise findings fixed by a push, even after hold.
    for held in [false, true] {
        let mut c = answered();
        c.push("push-after-look");
        if held {
            c.label(&["hold"]);
            assert!(matches!(c.pass(700).step, Step::Wait { .. }));
            assert!(c.memory.consumed_reviews.is_empty());
            c.label(&[]);
        }
        assert_eq!(
            c.pass(710).step,
            Step::Wait {
                until: Some(if held { 1000 } else { 1010 })
            }
        );
        assert_eq!(c.memory.consumed_reviews.len(), 1);
        assert_eq!(c.pass(1300).step, Step::QueueReview("reviewer".into()));
    }
}
#[test]
fn current_and_old_answers_still_post_when_the_reviewer_is_out() {
    // handle.py:120 / 132 / 142: unavailable reviewers' last answers must be posted.
    for push in [false, true] {
        let mut c = answered();
        c.service("reviewer").unavailable_until = Some(86400);
        if push {
            c.push("next-head");
        }
        assert_eq!(c.pass(700).step, Step::PostReview("reviewer".into()));
        assert_eq!(c.pass(800).step, Step::PostReview("reviewer".into()));
        assert!(c.memory.consumed_reviews.is_empty());
    }
}
#[test]
fn three_failed_posts_drop_an_answer_only_when_a_fresh_look_can_replace_it() {
    // handle.py:207 / 273: three failed posts in service requeue; findings remain owed on this head.
    let mut c = answered();
    c.failed_review = Some("reviewer".into());
    for at in [650, 710] {
        assert_eq!(c.pass(at).step, Step::PostReview("reviewer".into()));
    }
    assert_eq!(c.pass(770).step, Step::Wait { until: Some(4370) });
    c.failed_review = None;
    c.service("reviewer").unavailable_until = Some(86400);
    assert_eq!(c.pass(830).stage, Stage::Reviewer);
    c.push("head-it-never-read");
    // It may land on the available reviewer's word; here none is available.
    c.service("reviewer").unavailable_until = None;
    c.review("reviewer", Verdict::Clean, 900);
    assert_eq!(c.pass(1500).step, Step::Merge);
}
#[test]
fn failed_posts_are_kept_while_out_even_on_the_current_head() {
    // handle.py:223 / 240 / 257: no replacement look can come during outage.
    for push in [false, true] {
        let mut c = answered();
        c.service("reviewer").unavailable_until = Some(86400);
        if push {
            c.push("new-head");
        }
        c.failed_review = Some("reviewer".into());
        for at in [650, 710, 770, 830] {
            assert_eq!(c.pass(at).step, Step::PostReview("reviewer".into()));
        }
        assert_eq!(c.memory.strikes[&Failure::Post], 4);
        assert!(c.memory.consumed_reviews.is_empty());
    }
}
#[test]
fn a_current_clean_look_covers_an_old_answer_after_three_refused_posts() {
    // handle.py:207-239: only the same reviewer's clean current look covers its lost answer.
    let mut c = answered();
    c.push("current-head");
    c.review("reviewer", Verdict::Clean, 690);
    c.service("reviewer").unavailable_until = Some(86400);
    c.failed_review = Some("reviewer".into());
    assert_eq!(c.pass(700).step, Step::PostReview("reviewer".into()));
    assert_eq!(c.pass(760).step, Step::PostReview("reviewer".into()));
    assert_eq!(c.pass(820).step, Step::Merge);
}
#[test]
fn an_underway_unavailable_look_holds_but_an_unstarted_queue_does_not() {
    // handle.py:294 / 309 / 313: a look underway still answers; an unstarted queue is no verdict owed.
    let mut c = Case::new();
    c.roles(&["available", "out"]);
    c.review("available", Verdict::Clean, 600);
    c.service("out").unavailable_until = Some(86400);
    c.service("out").under_way = true;
    assert_eq!(c.view(700).stage, Stage::Review);
    c.service("out").under_way = false;
    c.service("out").queued = true;
    assert_eq!(c.view(700).step, Step::Merge);
}
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
        c.base_held = true;
        c.settings
            .proofs
            .insert("proof".into(), Recipe::Job("prove it".into()));
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

#[test]
fn three_review_failures_wait_an_hour_and_repeated_observations_count_once() {
    // passes.py:119 / handle.py: service failures retry an hour after the third, from its own time.
    let mut c = Case::new();
    c.roles(&["reviewer"]);
    for at in [500, 600, 650] {
        c.service("reviewer").failed_at = Some(at);
        c.pass(at);
    }
    assert_eq!(c.pass(800).step, Step::Wait { until: Some(4250) });
    assert_eq!(c.memory.review_failures.values().next().unwrap().0, 3);
    assert_eq!(c.pass(4250).step, Step::QueueReview("reviewer".into()));
}
#[test]
fn a_disappeared_queue_gets_a_second_look_and_a_failed_enqueue_gets_five_minutes() {
    // passes.py:78: service queue disappears with no answer; two reads before requeue.
    let mut c = Case::new();
    c.roles(&["reviewer"]);
    c.service("reviewer").missing = true;
    assert_eq!(c.pass(700).step, Step::Wait { until: Some(760) });
    assert_eq!(c.pass(760).step, Step::QueueReview("reviewer".into()));
    c.pass(800);
    assert_eq!(c.memory.review_failures.values().next().unwrap().0, 1);
    c.service("reviewer").missing = false;
    c.service("reviewer").queue_failed_at = Some(800);
    assert_eq!(c.pass(900).step, Step::Wait { until: Some(1100) });
    assert_eq!(c.pass(1100).step, Step::QueueReview("reviewer".into()));
}
