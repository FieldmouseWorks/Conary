//! The reviewer is Codex's GitHub connector: the flow reads its word from
//! what it posts on the PR and asks it for a look only when one is owed that
//! no push brings, as burrow's steward does (review_step.py).
use super::flow_case::Case;
use super::head;
use crate::flow::*;
use crate::sim::{CONNECTOR, REMI};
use serde_json::json;

/// No review of the head yet: what the connector posted is gone.
fn unreviewed() -> Case {
    let c = Case::new();
    c.hub.remove_flow_reviews(REMI, 3);
    c
}

#[test]
fn a_head_with_no_review_waits_then_asks_once_an_hour() {
    let mut c = unreviewed();
    // Just pushed: the connector reviews each push on its own.
    let quiet = c.pass(60);
    assert_eq!(
        (quiet.stage, quiet.step),
        (Stage::Review, Step::Wait { until: Some(1800) })
    );
    // Half an hour on, it's asked for.
    let asked = c.pass(1800);
    assert_eq!(asked.stage, Stage::Review);
    assert!(matches!(&asked.step, Step::AskReview(why) if why.contains(&head(3)[..7])));
    // Asked (as the executor records once GitHub took the comment): not
    // again within the hour, then again.
    c.memory.asked = Some((head(3), 1800));
    assert_eq!(
        c.pass(3000).step,
        Step::Wait {
            until: Some(1800 + 3600)
        }
    );
    assert!(matches!(c.pass(5400).step, Step::AskReview(_)));
    // Asked for another head counts for nothing on this one.
    c.memory.asked = Some((head(2), 5400));
    assert!(matches!(c.view(5401).step, Step::AskReview(_)));
}

#[test]
fn the_owner_hears_when_no_review_comes_for_two_hours() {
    let mut c = unreviewed();
    assert_eq!(c.pass(7199).owner_notice, None);
    assert_eq!(c.pass(7200).owner_notice, Some(OwnerNotice::NoReviewer));
    c.review("reviewer", Verdict::Running, 7200);
    assert_eq!(c.pass(7300).owner_notice, None);
}

/// An App opened it: the connector reviews none of its heads unasked.
fn an_apps() -> Case {
    let c = unreviewed();
    c.hub
        .edit_pull(REMI, 3, |p| p["user"]["type"] = json!("Bot"));
    c
}

#[test]
fn an_apps_head_is_asked_for_on_the_pass_that_first_sees_it() {
    let mut c = an_apps();
    let first = c.pass(60);
    assert_eq!(first.stage, Stage::Review);
    assert!(matches!(&first.step, Step::AskReview(why) if why.contains(&head(3)[..7])));
    // Still at most once an hour for the head.
    c.memory.asked = Some((head(3), 60));
    c.memory.first_asked = Some((head(3), 60));
    assert_eq!(
        c.pass(1800).step,
        Step::Wait {
            until: Some(60 + 3600)
        }
    );
    assert!(matches!(c.pass(3660).step, Step::AskReview(_)));
}

#[test]
fn a_persons_head_still_waits_quiet_before_its_ask() {
    let mut c = unreviewed();
    assert_eq!(c.pass(60).step, Step::Wait { until: Some(1800) });
}

#[test]
fn the_owner_hears_two_hours_after_the_first_ask_however_often_asked_again() {
    let mut c = an_apps();
    c.pass(60);
    c.memory.asked = Some((head(3), 60));
    c.memory.first_asked = Some((head(3), 60));
    // Asked again an hour on: the clock still runs from the first ask.
    c.memory.asked = Some((head(3), 3660));
    assert_eq!(c.pass(60 + 7199).owner_notice, None);
    assert_eq!(
        c.pass(60 + 7200).owner_notice,
        Some(OwnerNotice::NoReviewer)
    );
    // A person's head asked for after its quiet half hour: from that ask.
    let mut c = unreviewed();
    c.pass(0);
    c.memory.asked = Some((head(3), 1800));
    c.memory.first_asked = Some((head(3), 1800));
    assert_eq!(c.pass(7200).owner_notice, None);
    assert_eq!(
        c.pass(1800 + 7200).owner_notice,
        Some(OwnerNotice::NoReviewer)
    );
    // An ask of another head doesn't hold this one's clock.
    c.memory.first_asked = Some((head(2), 5000));
    assert_eq!(c.pass(7200).owner_notice, Some(OwnerNotice::NoReviewer));
}

#[test]
fn a_review_under_way_is_waited_for() {
    let c = unreviewed();
    c.hub.flow_review(REMI, 3, Verdict::Running, 100);
    let next = c.view(4000);
    assert_eq!(
        (next.stage, next.step),
        (Stage::Review, Step::Wait { until: None })
    );
    assert!(next.description.contains("is reviewing this commit"));
}

/// The connector's summary on Conary#511: completed, with no word on
/// findings and no review under way (#513).
#[test]
fn a_review_ended_with_no_word_on_findings_is_asked_for_again() {
    let mut c = unreviewed();
    c.hub.flow_summary(REMI, 3, "completed", None, 100);
    assert_eq!(c.pass(600).step, Step::Wait { until: Some(1800) });
    let asked = c.pass(1800);
    assert_eq!(asked.stage, Stage::Review);
    assert!(matches!(&asked.step, Step::AskReview(_)));
    assert!(!asked.description.contains("is reviewing this commit"));
    assert_eq!(c.pass(7200).owner_notice, Some(OwnerNotice::NoReviewer));
    // An App's head is asked for on the first pass, as with no summary.
    let mut c = an_apps();
    c.hub.flow_summary(REMI, 3, "completed", None, 30);
    assert!(matches!(c.pass(60).step, Step::AskReview(_)));
}

#[test]
fn a_clean_review_of_the_head_settles_it() {
    let c = unreviewed();
    c.hub.flow_review(REMI, 3, Verdict::Clean, 100);
    assert_eq!(c.view(4000).step, Step::Merge);
}

#[test]
fn a_clean_review_of_another_commit_says_nothing_of_the_head() {
    let c = Case::new();
    c.push(&head(4));
    // The flow first sees the push now.
    let next = c.view(4000);
    assert_eq!(
        (next.stage, next.step),
        (
            Stage::Review,
            Step::Wait {
                until: Some(4000 + 1800)
            }
        )
    );
}

#[test]
fn findings_with_open_threads_go_to_a_fix() {
    let c = unreviewed();
    c.hub.flow_review(REMI, 3, Verdict::Findings, 100);
    c.threads(2);
    let next = c.view(4000);
    assert_eq!(
        (next.stage, next.step),
        (Stage::Fix, Step::HandFix(Kind::Findings))
    );
}

#[test]
fn answered_findings_ask_for_another_look_once_settled() {
    let mut c = unreviewed();
    c.hub.flow_review(REMI, 3, Verdict::Findings, 4000);
    c.threads(1);
    c.answered();
    // Answered a moment ago: the answers settle first.
    let settling = c.pass(4000 + 30);
    assert_eq!(settling.stage, Stage::Review);
    assert_eq!(
        settling.step,
        Step::Wait {
            until: Some(4000 + 300 + 1)
        }
    );
    assert!(settling.description.contains("review round 2 next"));
    let asked = c.pass(4000 + 301);
    assert_eq!(asked.stage, Stage::Review);
    assert!(
        matches!(&asked.step, Step::AskReview(why) if why.contains("Every finding is answered"))
    );
}

#[test]
fn a_clean_look_after_findings_on_the_same_head_settles_it() {
    let c = unreviewed();
    c.hub.flow_review(REMI, 3, Verdict::Findings, 100);
    c.threads(1);
    c.answered();
    c.hub.flow_review(REMI, 3, Verdict::Clean, 200);
    assert_eq!(c.view(4000).step, Step::Merge);
}

#[test]
fn findings_past_the_round_limit_go_to_a_judge() {
    let mut c = unreviewed();
    c.settings.rounds = 2;
    c.hub.flow_review(REMI, 3, Verdict::Findings, 100);
    c.push(&head(4));
    c.hub.flow_review(REMI, 3, Verdict::Findings, 200);
    c.threads(1);
    c.answered();
    assert_eq!(c.pass(4000).step, Step::Judge(vec![Trigger::RoundLimit]));
}

#[test]
fn a_pr_changed_after_its_push_is_judged_not_landed_on_the_reviewers_word() {
    for (since, judged) in [
        // Retargeted after the push, or edited, or with no push time.
        (reviews(None, Some(200), Some(100)), true),
        (reviews(Some(200), None, Some(100)), true),
        (reviews(Some(100), None, Some(100)), true),
        (reviews(Some(50), None, None), true),
        // Before the push: every review of the push read it.
        (reviews(Some(50), Some(50), Some(100)), false),
        (reviews(None, None, None), false),
    ] {
        let c = Case::new();
        c.hub.edit_flow(REMI, 3, |s| s.since = since);
        let step = c.view(4000).step;
        if judged {
            assert_eq!(step, Step::Judge(vec![Trigger::Changed]), "{since:?}");
        } else {
            assert_eq!(step, Step::Merge, "{since:?}");
        }
    }
}

fn reviews(edited: Option<u64>, retargeted: Option<u64>, pushed: Option<u64>) -> Since {
    Since {
        edited,
        retargeted,
        pushed,
    }
}

#[test]
fn only_the_connectors_own_posts_count() {
    // Anyone can write the marks, so a PR's author could call its own head
    // clean, or count rounds against it.
    let c = unreviewed();
    let h = head(3);
    let state =
        json!({"code_review":{"commit_sha":h,"has_any_findings":false,"status":"completed"}});
    c.hub.flow_comment(
        REMI,
        3,
        "mallory",
        &format!("<!-- codex-review-state:v1 {state} -->"),
    );
    c.hub.person_review(
        REMI,
        3,
        "mallory",
        "User",
        &h,
        &format!("**Reviewed commit:** `{}`", &h[..10]),
        "COMMENTED",
    );
    let f = c.facts();
    assert_eq!(f.reviewers["reviewer"], Reviewer::default());
    assert!(matches!(c.view(4000).step, Step::AskReview(_)));
    // The connector's own say-so counts.
    c.hub.flow_comment(
        REMI,
        3,
        CONNECTOR,
        &format!("<!-- codex-review-state:v1 {state} -->"),
    );
    assert_eq!(c.view(4000).step, Step::Merge);
}

#[test]
fn every_findings_review_is_a_round_at_any_head() {
    let c = unreviewed();
    c.hub.flow_review(REMI, 3, Verdict::Findings, 100);
    c.push(&head(4));
    c.hub.flow_review(REMI, 3, Verdict::Findings, 200);
    c.push(&head(5));
    let r = &c.facts().reviewers["reviewer"];
    assert_eq!((r.verdict, r.rounds), (None, 2));
}
