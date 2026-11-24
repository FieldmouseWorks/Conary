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
    c.answered();
    assert_eq!(c.pass(4001).step, Step::Judge(vec![Trigger::RoundLimit]));
    c.judge("pass");
    assert_eq!(c.view(4002).step, Step::Merge);
}
#[test]
fn a_push_past_the_limit_is_judged_not_reviewed_again() {
    // rounds.py:33-43, a fresh head retains the global round count.
    let mut c = five();
    c.answered();
    c.push(&head(4));
    assert_eq!(c.pass(4000).step, Step::Judge(vec![Trigger::RoundLimit]));
    c.judge("fail");
    assert_eq!(c.pass(4001).step, Step::HandFix(Kind::JudgeFailure));
    c.judge("owner");
    assert_eq!(c.view(4002).step, Step::ListOwner(Door::Judge));
}
#[test]
fn current_clean_reviews_override_an_earlier_cap() {
    // rounds.py:44-47: a clean look arriving anyway still covers the head.
    let mut c = five();
    c.answered();
    c.review("primary", Verdict::Clean, 10);
    c.review("secondary", Verdict::Clean, 10);
    assert_eq!(c.view(4000).step, Step::Merge);
}
#[test]
fn answered_findings_wait_for_a_new_look_below_the_cap() {
    // review_step.py: every finding answered without a push asks the reviewer again.
    let mut c = Case::new();
    c.review("reviewer", Verdict::Findings, 1000);
    c.answered();
    assert_eq!(c.view(1200).step, Step::Wait { until: Some(1301) });
    assert!(matches!(c.view(1301).step, Step::AskReview(_)));
}

#[test]
fn an_authors_r2_raise_cannot_be_taken_back_with_the_label() {
    // gates.py / Conary's class: author raises persist across passes and heads.
    let mut c = Case::new();
    c.label(&["r2"]);
    assert_eq!(c.pass(4000).step, Step::Judge(vec![Trigger::R2]));
    c.label(&[]);
    c.judge("owner");
    assert_eq!(c.pass(4001).step, Step::ListOwner(Door::Judge));
    c.push(&head(4));
    c.review("reviewer", Verdict::Clean, 4002);
    assert_eq!(c.pass(5000).step, Step::Judge(vec![Trigger::R2]));
    c.judge("pass");
    assert_eq!(c.view(5001).step, Step::Merge);
}
#[test]
fn a_review_raise_survives_label_removal_on_documentation() {
    // gates.py: an author may ask for more review, never waive an observed raise.
    let mut c = Case::new();
    c.hub.touch(&head(3), &["docs/a.md"]);
    c.hub.remove_flow_reviews(REMI, 3);
    c.label(&["review"]);
    assert!(matches!(c.pass(4000).step, Step::AskReview(_)));
    c.label(&[]);
    assert!(matches!(c.view(4001).step, Step::AskReview(_)));
}
