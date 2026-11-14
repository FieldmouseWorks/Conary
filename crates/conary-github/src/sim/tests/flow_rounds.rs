//! Executable review-round scenarios in burrow's rounds.py (7841daf).
use super::{flow_case::Case, head};
use crate::flow::*;
use crate::sim::REMI;

fn five() -> Case {
    let mut c = Case::new();
    c.roles(&["primary", "secondary"]);
    for t in 1..=5 {
        c.review("primary", Verdict::Findings, t);
    }
    c
}
#[test]
fn past_the_limit_fixes_findings_then_judges_answered_findings() {
    // rounds.py:16-32: cap stops the reviewers, never hands the limit to owner.
    let mut c = five();
    c.threads(1);
    assert_eq!(c.pass(4000).step, Step::HandFix(Kind::Findings));
    assert!(
        c.view(4000)
            .description
            .contains("Past the review-round limit (5)")
    );
    c.answered("primary");
    assert_eq!(c.pass(4001).step, Step::Judge(vec![Trigger::RoundLimit]));
    c.judge("pass");
    assert_eq!(c.view(4002).step, Step::Merge);
}
#[test]
fn a_push_past_the_limit_is_judged_not_reviewed_again() {
    // rounds.py:33-43, a fresh head retains the global round count.
    let mut c = five();
    c.answered("primary");
    c.push(&head(4));
    assert_eq!(c.pass(4000).step, Step::Judge(vec![Trigger::RoundLimit]));
    c.judge("fail");
    assert_eq!(c.pass(4001).step, Step::HandFix(Kind::JudgeFailure));
    c.judge("owner");
    assert_eq!(c.view(4002).step, Step::ListOwner);
}
#[test]
fn current_clean_reviews_override_an_earlier_cap() {
    // rounds.py:44-47: a clean look arriving anyway still covers the head.
    let mut c = five();
    c.answered("primary");
    c.review("primary", Verdict::Clean, 10);
    c.review("secondary", Verdict::Clean, 10);
    assert_eq!(c.view(4000).step, Step::Merge);
}
#[test]
fn an_already_queued_look_is_owed_even_after_a_land_verdict() {
    // rounds.py:49-62, no judge lands past the final look still owed.
    let mut c = five();
    c.answered("primary");
    c.service("secondary").queued = true;
    assert_eq!(c.pass(4000).stage, Stage::Review);
    c.judge("pass");
    assert!(!matches!(c.pass(4001).step, Step::Merge | Step::Judge(_)));
    c.service("secondary").queued = false;
    c.review("secondary", Verdict::Findings, 20);
    assert_eq!(c.view(4002).step, Step::HandFix(Kind::Findings));
}
#[test]
fn lost_findings_are_owed_on_the_same_base_after_a_push() {
    // rounds.py:63-82 and 98-114: a loss is the same round on the new head.
    let mut c = five();
    c.answered("primary");
    c.memory
        .lost_on
        .entry("primary".into())
        .or_default()
        .insert("main".into(), head(3).into());
    c.judge("pass");
    assert_eq!(c.pass(4000).step, Step::QueueReview("primary".into()));
    c.push(&head(4));
    assert!(matches!(
        c.pass(4001).step,
        Step::Wait { until: Some(4301) }
    ));
    assert_eq!(c.pass(4301).step, Step::QueueReview("primary".into()));
    c.review("primary", Verdict::Clean, 4400);
    assert_eq!(c.pass(4500).step, Step::Judge(vec![Trigger::RoundLimit]));
    assert!(c.memory.lost_on.is_empty());
}
#[test]
fn an_earlier_head_in_flight_is_posted_past_the_limit() {
    // rounds.py:83-97: queued at this or an earlier head, begun or not.
    for old in [head(3), head(2)] {
        let mut c = five();
        c.answered("primary");
        c.service("primary").queued = true;
        assert_eq!(c.pass(4000).stage, Stage::Review);
        let mut revision = c.scope();
        revision.head = old;
        c.service("primary").answer = Some(Review {
            revision,
            verdict: Verdict::Findings,
            at: 4001,
        });
        c.judge("pass");
        assert_eq!(c.view(4002).step, Step::PostReview("primary".into()));
    }
}
#[test]
fn an_older_clean_look_cannot_cover_a_later_loss_on_this_head() {
    // rounds.py:115-144: posting an old clean look does not clear the new loss.
    let mut c = five();
    c.answered("primary");
    c.review("primary", Verdict::Clean, 20);
    c.pass(4000); // remember the clean review that already exists
    c.memory
        .lost_on
        .entry("primary".into())
        .or_default()
        .insert("main".into(), head(3).into());
    assert_eq!(c.pass(4001).step, Step::QueueReview("primary".into()));
    assert!(c.memory.lost_on.contains_key("primary"));
    c.review("primary", Verdict::Clean, 30);
    c.pass(4002);
    assert!(c.memory.lost_on.contains_key("primary")); // new ID, but still an older look
    c.review("primary", Verdict::Clean, 4003);
    c.pass(4004);
    assert!(!c.memory.lost_on.contains_key("primary"));
}
#[test]
fn losses_on_another_base_are_not_cleared_by_this_bases_clean_review() {
    // rounds.py:153-180: a loss is owed if the PR returns to that base.
    let mut c = five();
    c.answered("primary");
    c.review("primary", Verdict::Clean, 10);
    let mut other = c.scope();
    other.base = "release".into();
    c.service("primary").answer = Some(Review {
        revision: other,
        verdict: Verdict::Findings,
        at: 4000,
    });
    c.pass(4001);
    assert_eq!(c.memory.lost_on["primary"]["release"].head(), head(3));
    c.hub.set_branch(REMI, "release", super::TRUNK);
    c.hub.retarget(REMI, 3, "release");
    c.hub.refresh_flow(REMI, 3);
    c.service("primary").answer = None;
    assert_eq!(c.pass(4002).step, Step::QueueReview("primary".into()));
}
#[test]
fn the_cap_survives_outages_including_every_reviewer_being_out() {
    // rounds.py:181-190: no available reviewer does not hand the cap to owner.
    for all in [true, false] {
        let mut c = five();
        c.answered("primary");
        c.service("primary").unavailable_until = Some(9000);
        if all {
            c.service("secondary").unavailable_until = Some(9000);
        }
        assert_eq!(c.view(4000).step, Step::Judge(vec![Trigger::RoundLimit]));
    }
}
#[test]
fn docs_still_owe_lost_findings_from_an_unavailable_reviewer() {
    // review_step.py / lost.py: no review-round waiver drops owed findings.
    let mut c = Case::new();
    c.roles(&["primary"]);
    c.hub.touch(&head(3), &["docs/a.md"]);
    c.memory
        .lost_on
        .entry("primary".into())
        .or_default()
        .insert("main".into(), head(2).into());
    c.service("primary").unavailable_until = Some(9000);
    assert_eq!(c.view(4000).stage, Stage::Reviewer);
}
#[test]
fn answered_findings_wait_for_a_new_look_below_the_cap() {
    // handle.py: every finding answered without a push asks the reviewers again.
    let mut c = Case::new();
    c.review("reviewer", Verdict::Findings, 1000);
    c.answered("reviewer");
    assert_eq!(c.view(1200).step, Step::Wait { until: Some(1301) });
    assert_eq!(c.view(1301).step, Step::QueueReview("reviewer".into()));
}
#[test]
fn unseen_snapshot_paths_still_need_a_judge_after_clean_reviews() {
    // review.py:178-189: hidden-path evidence survives a lost local snapshot.
    let mut c = Case::new();
    c.unseen = true;
    assert_eq!(c.view(4000).step, Step::Judge(vec![Trigger::HiddenFiles]));
}

#[test]
fn an_authors_r2_raise_cannot_be_taken_back_with_the_label() {
    // gates.py / Conary's class: author raises persist across passes and heads.
    let mut c = Case::new();
    c.label(&["r2"]);
    assert_eq!(c.pass(4000).step, Step::Judge(vec![Trigger::R2]));
    c.label(&[]);
    c.judge("pass");
    assert_eq!(c.pass(4001).step, Step::ListOwner);
    c.push(&head(4));
    c.review("reviewer", Verdict::Clean, 4002);
    c.judge("pass");
    assert_eq!(c.view(5000).step, Step::ListOwner);
}
#[test]
fn a_review_raise_survives_label_removal_on_documentation() {
    // gates.py: an author may ask for more review, never waive an observed raise.
    let mut c = Case::new();
    c.hub.touch(&head(3), &["docs/a.md"]);
    c.hub.remove_flow_reviews(REMI, 3);
    c.label(&["review"]);
    assert_eq!(c.pass(4000).step, Step::QueueReview("reviewer".into()));
    c.label(&[]);
    assert_eq!(c.view(4001).step, Step::QueueReview("reviewer".into()));
}

#[test]
fn three_refused_current_posts_still_get_a_replacement_look_past_the_cap() {
    // rounds.py:61: a lost current answer is the same owed round, even at the limit.
    let mut c = Case::new();
    c.roles(&["reviewer"]);
    for at in 1..=5 {
        c.review("reviewer", Verdict::Findings, at);
    }
    c.answered("reviewer");
    c.service("reviewer").answer = Some(Review {
        revision: c.scope(),
        verdict: Verdict::Findings,
        at: 600,
    });
    c.failed_review = Some("reviewer".into());
    for at in [650, 710] {
        assert_eq!(c.pass(at).step, Step::PostReview("reviewer".into()));
    }
    assert_eq!(c.pass(770).step, Step::Wait { until: Some(4371) });
    c.failed_review = None;
    assert_eq!(c.memory.lost_on["reviewer"]["main"].head(), c.scope().head);
    assert_eq!(c.pass(4470).step, Step::QueueReview("reviewer".into()));
}
