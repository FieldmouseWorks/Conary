use super::flow_case::Case;
use crate::branches::Branches;
use crate::flow::*;
use crate::sim::{REMI, faults::Call};

pub(super) fn merge_base_into(c: &Case, changed: bool) -> String {
    let old = c.scope().head;
    let trunk = c.hub.branch(REMI, "main").unwrap().unwrap();
    let tip = c
        .hub
        .add(&trunk, "line13: trunk moved beside the PR's line");
    c.hub.set_branch(REMI, "main", &tip);
    let mut patches = c.hub.patches(&tip);
    patches.extend(c.hub.patches(&old));
    if changed {
        patches.insert("line15: conflict resolution nobody reviewed".into());
    }
    let sha = crate::sim::tree_of(&["merge-of-main", &old, &tip]);
    c.hub.commit_as(&sha, &[old, tip], patches);
    c.push(&sha);
    sha
}
#[test]
fn a_base_only_merge_carries_clean_reviews_without_another_round() {
    // carried.py:70: base moves in the same file, merge changes none of the PR's edits.
    let mut c = Case::new();
    merge_base_into(&c, false);
    c.pass(0);
    assert_eq!(c.view(700).step, Step::Merge);
}
#[test]
fn latest_findings_invalidate_an_earlier_clean_carried_review() {
    // carried.py:80: latest findings on the reviewed head prevent its older clean look carrying.
    let mut c = Case::new();
    c.roles(&["reviewer", "other"]);
    c.review("reviewer", Verdict::Clean, 500);
    c.review("other", Verdict::Clean, 500);
    c.review("reviewer", Verdict::Findings, 600);
    merge_base_into(&c, false);
    c.pass(0);
    assert_eq!(c.view(8000).step, Step::HandFix(Kind::Findings));
    assert_eq!(
        c.facts().reviewers["other"]
            .review
            .as_ref()
            .unwrap()
            .verdict,
        Verdict::Clean
    );
}
#[test]
fn a_merge_resolution_is_a_new_change_and_cannot_carry_reviews() {
    // carried.py:89: resolving the PR's own changed line requires fresh reviews.
    let mut c = Case::new();
    merge_base_into(&c, true);
    c.pass(0);
    assert_eq!(c.view(700).step, Step::QueueReview("reviewer".into()));
}
#[test]
fn hidden_paths_still_require_a_judge_when_their_looks_carry() {
    // carried.py:102: carrying clean looks doesn't expose a path the reviewers were not shown.
    let mut c = Case::new();
    c.hub.touch(&c.scope().head, &["hidden/key.pem"]);
    merge_base_into(&c, false);
    c.pass(0);
    assert!(c.facts().unseen);
    assert_eq!(c.view(700).step, Step::Judge);
}
#[test]
fn unreadable_carried_history_never_becomes_a_clean_review() {
    // carried.py / carry.rs: an unreadable earlier head cannot establish equivalent trees.
    let mut c = Case::new();
    merge_base_into(&c, false);
    c.pass(0);
    c.hub.fail(Call::Carries);
    assert_eq!(c.view(700).step, Step::QueueReview("reviewer".into()));
}
