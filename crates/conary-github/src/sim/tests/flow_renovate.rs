use super::flow_case::Case;
use crate::flow::*;
use crate::sim::REMI;
use serde_json::json;

fn renovate() -> Case {
    let mut c = Case::new();
    c.settings.renovate_email = Some("bot@example.org".into());
    c.settings.renovate_login = Some("renovate[bot]".into());
    c.hub.edit_pull(REMI, 3, |p| {
        p["head"]["ref"] = json!("renovate/foo-3");
        p["title"] = json!("Update foo from 1.4 to 1.5");
    });
    c.hub.refresh_flow(REMI, 3);
    c.hub.commit_details(
        &c.scope().head,
        Some("deps: bump"),
        Some("bot@example.org"),
        Some("bot@example.org"),
    );
    c.threads(1);
    c
}

#[test]
fn a_standing_rebase_request_fences_findings_handoffs() {
    // deps.py:190 / 233: both the rebase label and rebase! title own the branch.
    for title in [false, true] {
        let c = renovate();
        if title {
            c.hub.edit_pull(REMI, 3, |p| {
                p["title"] = json!("rebase!Update foo from 1.4 to 1.5")
            });
            c.hub.refresh_flow(REMI, 3);
        } else {
            c.label(&["rebase"]);
        }
        assert_eq!(c.view(8000).stage, Stage::Fix);
        assert!(matches!(c.view(8000).step, Step::Wait { .. }));
    }
}

#[test]
fn removed_rebase_requests_fence_for_a_full_run_across_pushes() {
    // deps.py:197-223 / 233: timeline removal, even unseen, not the steward's last observation.
    let mut c = renovate();
    c.hub.edit_flow(REMI, 3, |s| s.rebase_removed = vec![10700]);
    assert_eq!(c.pass(14500).step, Step::Wait { until: Some(17900) });
    c.push("renovates-intermediate-push");
    c.hub.commit_details(
        &c.scope().head,
        Some("deps: bump"),
        Some("bot@example.org"),
        Some("bot@example.org"),
    );
    assert_eq!(c.pass(14900).step, Step::Wait { until: Some(17900) });
    assert_eq!(c.pass(17901).step, Step::HandFix(Kind::Findings));
}

#[test]
fn a_truncated_request_history_expires_from_its_oldest_event() {
    // deps.py:247: a request may be in the cut history, until a full run passes.
    let c = renovate();
    c.hub.edit_flow(REMI, 3, |s| s.timeline_cut_at = Some(7000));
    assert_eq!(c.view(7200).step, Step::Wait { until: Some(14200) });
    assert_eq!(c.view(14201).step, Step::HandFix(Kind::Findings));
    // deps.py:225: unrelated label removals are not a request.
    c.hub.edit_flow(REMI, 3, |s| s.timeline_cut_at = None);
    assert_eq!(c.view(7300).step, Step::HandFix(Kind::Findings));
}

#[test]
fn a_red_edited_dependency_still_belongs_to_the_sweep() {
    // deps.py:294: even a human-edited Renovate branch returns to the sweep when red.
    let c = renovate();
    c.hub.commit_details(
        &c.scope().head,
        Some("repair: fix"),
        Some("person@example.org"),
        Some("person@example.org"),
    );
    c.hub.flow_review(REMI, 3, "reviewer", Verdict::Clean, 700);
    c.ci("failure");
    assert_eq!(c.view(8000).stage, Stage::Fix);
    assert!(matches!(c.view(8000).step, Step::Wait { .. }));
    c.ci("success");
    assert_eq!(c.view(8000).step, Step::HandFix(Kind::Findings));
}

fn artifacts(c: &Case) {
    c.hub.put_statuses(&c.scope().head, json!([{"context":"renovate/artifacts","state":"failure","description":"artifact failure"}]));
}

#[test]
fn release_age_requires_every_failure_block_to_end_in_the_age_reason() {
    // deps.py:156-182: anything else red in the comment is a real failure.
    let waiting = "lock-age: nothing in the refresh is old enough until 2030-01-02T00:00:00Z";
    for (blocks, age) in [
        (format!("```\n{waiting}\n```"), true),
        (format!("```\n{waiting}\nnetwork failed\n```"), false),
        (
            format!("```\n{waiting}\n```\n```\nnetwork failed\n```"),
            false,
        ),
        (format!("```\n{waiting}\n```\n```\n{waiting}\n```"), true),
        (format!("```\nquoted {waiting}\n```"), false),
    ] {
        let c = renovate();
        c.threads(0);
        artifacts(&c);
        c.hub.flow_comment(
            REMI,
            3,
            "renovate[bot]",
            &format!("Artifact update problem\n{blocks}"),
        );
        assert_eq!(c.view(8000).stage == Stage::ReleaseAge, age, "{blocks}");
    }
}
