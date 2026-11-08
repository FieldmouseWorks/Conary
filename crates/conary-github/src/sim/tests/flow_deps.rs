use super::flow_case::Case;
use crate::flow::*;
use crate::sim::REMI;
use serde_json::json;
fn bot(renovate: bool, major: bool) -> Case {
    let mut c = Case::new();
    c.settings.renovate_email = Some("bot@renovateapp.com".into());
    c.settings.renovate_login = Some("renovate[bot]".into());
    c.hub.edit_pull(REMI, 3, |p| {
        p["head"]["ref"] = json!(if renovate {
            "renovate/foo"
        } else {
            "dependabot/foo"
        });
        p["title"] = json!(if major {
            "Update foo from 1.4 to 2.0"
        } else {
            "Update foo from 1.4 to 1.5"
        });
    });
    c.hub.refresh_flow(REMI, 3);
    let email = if renovate {
        "bot@renovateapp.com"
    } else {
        "49699333+dependabot[bot]@users.noreply.github.com"
    };
    c.hub.commit_details(
        &c.scope().head,
        Some("deps: bump"),
        Some(email),
        Some(email),
    );
    c
}
#[test]
fn automerge_configuration_gives_only_the_documented_batches_to_the_steward() {
    // deps.py:59,63,74,83: batch labels and configuration, including conflicting quoted statuses.
    let spent = "🚦 **Automerge**: Disabled because a matching PR was automerged previously.";
    for (label, body, expected) in [
        (false, "🚦 **Automerge**: Enabled.", Step::Settle),
        (true, "🚦 **Automerge**: Enabled.", Step::Merge),
        (true, "🚦 **Automerge**: Disabled by config.", Step::Settle),
        (false, spent, Step::Merge),
        (true, spent, Step::Merge),
    ] {
        let c = bot(true, false);
        if label {
            c.label(&["upstream batch"]);
        }
        c.description(body, 0);
        assert_eq!(c.view(8000).step, expected);
    }
    for enabled in [false, true] {
        let c = bot(true, false);
        c.description(
            &format!(
                "### Release Notes\n\n{spent}\n\n---\n\n### Configuration\n\n🚦 **Automerge**: {}.",
                if enabled {
                    "Enabled"
                } else {
                    "Disabled by config"
                }
            ),
            0,
        );
        assert_eq!(c.view(8000).step, Step::Settle);
    }
}
#[test]
fn dependency_edits_are_reviewed_for_both_author_and_committer_changes() {
    // deps.py:97,110,287,306,318,344,366,371: bot identity is machine configuration and both addresses count.
    for renovate in [false, true] {
        for changed_author in [false, true] {
            let mut c = bot(renovate, false);
            let email = if renovate {
                "bot@renovateapp.com"
            } else {
                "49699333+dependabot[bot]@users.noreply.github.com"
            };
            c.roles(&["reviewer"]);
            c.hub.commit_details(
                &c.scope().head,
                Some("repair: fix"),
                Some(if changed_author { "person" } else { email }),
                Some(if changed_author { email } else { "person" }),
            );
            assert_eq!(c.view(8000).step, Step::QueueReview("reviewer".into()));
            c.review("reviewer", Verdict::Clean, 700);
            assert_eq!(c.view(8000).step, Step::Merge);
        }
    }
    let mut c = bot(false, false);
    c.settings.renovate_email = None;
    assert_eq!(c.view(8000).step, Step::Merge);
    let mut c = bot(true, false);
    c.settings.renovate_email = None;
    assert_eq!(c.facts().bot_edited, None);
    // Unknown/misconfigured provenance never gets the CI-only shortcut.
    assert_eq!(c.view(8000).step, Step::QueueReview("reviewer".into()));
    c.review("reviewer", Verdict::Clean, 700);
    assert_eq!(c.view(8000).step, Step::Merge); // current trusted reviewer covers the complete claim
}
#[test]
fn majors_threads_and_security_use_their_own_gates() {
    // deps.py:41,45,59,126,136,144,149,259,265,270,282.
    let c = bot(false, true);
    assert_eq!(c.view(8000).step, Step::Judge);
    let mut c = bot(true, true);
    c.settings.renovate_sweep = true;
    assert_eq!(c.view(8000).stage, Stage::Review);
    c.threads(1);
    assert!(matches!(c.view(8000).step, Step::Wait { .. }));
    c.sweep("held");
    assert_eq!(c.view(8000).step, Step::ListOwner);
    c.sweep("ok");
    assert_eq!(c.view(8000).step, Step::HandFix(Kind::Findings));
    c.label(&["needs-peter"]);
    assert_eq!(c.view(8000).step, Step::ListOwner);
    c.label(&["security"]);
    c.threads(0);
    assert_eq!(c.facts().dependency, Some(Dependency::Minor));
    assert_eq!(c.view(8000).step, Step::Settle);
    let c = bot(false, false);
    c.threads(1);
    assert_eq!(c.view(8000).step, Step::HandFix(Kind::Findings));
    c.threads(0);
    c.ci("failure");
    assert_eq!(c.view(8000).step, Step::HandFix(Kind::Red));
}
