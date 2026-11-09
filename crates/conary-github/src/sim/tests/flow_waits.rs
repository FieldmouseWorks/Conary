use super::flow_case::Case;
use crate::flow::*;
use crate::sim::REMI;
use serde_json::json;
#[test]
fn reviewer_recovery_and_quiet_wakeups_do_not_extend_the_stall_deadline() {
    // watched.py:67 / 86: wait deadlines follow phase/revision, not scheduled rechecks.
    let mut c = Case::new();
    c.roles(&["reviewer", "covering"]);
    c.review("covering", Verdict::Clean, 600);
    c.memory.lost_on.insert(
        "reviewer".into(),
        std::collections::BTreeMap::from([("main".into(), c.scope().head.into())]),
    );
    c.service("reviewer").unavailable_until = Some(86400);
    let n = c.pass(700);
    assert_eq!(n.overdue_at, Some(7900));
    assert_eq!(n.wait_on, Some(WaitOn::Steward));
    assert_eq!(c.pass(800).overdue_at, n.overdue_at);
    c.threads(1);
    let n = c.pass(810);
    assert_eq!(n.overdue_at, Some(8010));
    assert_eq!(n.step, Step::Wait { until: Some(1800) });
}
#[test]
fn release_age_and_refresh_recovery_have_the_sweeps_full_allowance() {
    // watched.py:97-140: max(since, ripe) + sweep recovery + stall, or drain for other red.
    let mut c = Case::new();
    c.settings.renovate_sweep = true;
    c.settings.renovate_login = Some("renovate[bot]".into());
    c.settings.renovate_email = Some("bot@example.org".into());
    c.hub.edit_pull(REMI, 3, |p| {
        p["head"]["ref"] = json!("renovate/cargo-lock-file-maintenance");
        p["title"] = json!("Lock file maintenance (cargo)");
    });
    c.hub.refresh_flow(REMI, 3);
    c.hub.commit_details(
        &c.scope().head,
        Some("deps: refresh"),
        Some("bot@example.org"),
        Some("bot@example.org"),
    );
    c.hub.put_statuses(
        &c.scope().head,
        json!([{"context":"renovate/artifacts","state":"failure"}]),
    );
    c.hub.flow_comment(REMI,3,"renovate[bot]","Artifact update problem\n```\nlock-age: nothing in the refresh is old enough until 2030-01-02T00:00:00Z\n```");
    let n = c.pass(700);
    let ripe = c.facts().release_at.unwrap();
    let drain = 4 * (3600 + 7200) + 7200;
    let recovery = 3 * (21600 + 3600 + 7200) + drain;
    assert_eq!(n.overdue_at, Some(ripe + recovery + 7200));
    assert_eq!(n.wait_on, Some(WaitOn::Sweep));
    c.label(&["rebase"]);
    assert_eq!(c.pass(800).overdue_at, n.overdue_at);
    c.hub.flow_comment(
        REMI,
        3,
        "renovate[bot]",
        "Artifact update problem\n```\nnetwork failed\n```",
    );
    assert_eq!(c.pass(900).overdue_at, Some(900 + drain + 7200));
}
#[test]
fn only_known_live_job_work_waits_on_engine_usage() {
    // lost.py:270: queued/running job, never unknown or machine proof builds.
    for (state, usage) in [
        (JobState::Queued, true),
        (JobState::Running, true),
        (JobState::Unknown, false),
        (JobState::Done, false),
        (JobState::Broken, false),
    ] {
        let mut c = Case::new();
        c.threads(1);
        c.jobs.insert(
            Kind::Findings,
            Job {
                id: "fix".into(),
                revision: c.scope(),
                state,
                started: 700,
                repeated: false,
            },
        );
        assert_eq!(c.view(800).on_usage, usage);
    }
}

#[test]
fn an_old_pending_pr_is_listed_but_held_draft_and_swept_prs_are_not() {
    // handle.py / conflicts.py / watched.py: open a day means listed, even if a check never reports.
    let mut c = Case::new();
    c.hub
        .edit_pull(REMI, 3, |p| p["created_at"] = json!("1970-01-01T00:00:00Z"));
    c.hub.refresh_flow(REMI, 3);
    c.hub.put_checks(
        &c.scope().head,
        json!([{"id":1,"name":"ci","status":"in_progress","conclusion":null}]),
    );
    let n = c.pass(86401);
    assert_eq!(n.owner_notice, Some(OwnerNotice::OpenOverDay));
    assert_eq!(n.overdue_at, None);
    c.label(&["hold"]);
    assert_eq!(c.view(86402).owner_notice, None);
    c.label(&[]);
    c.hub.edit_flow(REMI, 3, |s| s.draft = true);
    assert_eq!(c.view(86403).owner_notice, None);
}
#[test]
fn unavailable_and_repeatedly_failing_reviewers_are_listed_without_watching_them() {
    // watched.py:67 / passes.py:119: listed PRs are removed from watchdog waits.
    let mut c = Case::new();
    c.roles(&["reviewer"]);
    c.service("reviewer").unavailable_until = Some(86400);
    let n = c.view(700);
    assert_eq!(n.owner_notice, Some(OwnerNotice::NoReviewer));
    assert_eq!(n.overdue_at, None);
    c.service("reviewer").unavailable_until = None;
    for at in [500, 600, 650] {
        c.service("reviewer").failed_at = Some(at);
        c.pass(at);
    }
    let n = c.view(800);
    assert_eq!(n.owner_notice, Some(OwnerNotice::ReviewFailures));
    assert_eq!(n.overdue_at, None);
}

#[test]
fn a_settled_bot_waits_on_renovate_only_when_its_run_exists_on_the_machine() {
    // watched.py:259,272: enabled automerge uses Renovate's deadline; no local runner kicks steward.
    for (enabled, here, who, duration) in [
        (true, true, WaitOn::Renovate, 3600),
        (false, true, WaitOn::Steward, 7200),
        (true, false, WaitOn::Steward, 3600),
    ] {
        let mut c = Case::new();
        c.settings.renovate_sweep = here;
        c.settings.renovate_merge_after = 3600;
        c.settings.renovate_email = Some("bot@example.org".into());
        c.hub.edit_pull(REMI, 3, |p| {
            p["head"]["ref"] = json!("renovate/foo");
            p["title"] = json!("Update foo to v1.5");
        });
        c.hub.refresh_flow(REMI, 3);
        c.hub.commit_details(
            &c.scope().head,
            Some("deps: bump"),
            Some("bot@example.org"),
            Some("bot@example.org"),
        );
        c.description(
            if enabled {
                "🚦 **Automerge**: Enabled."
            } else {
                "🚦 **Automerge**: Disabled by config."
            },
            0,
        );
        let n = c.pass(450);
        assert_eq!(n.step, Step::Settle);
        assert_eq!(n.wait_on, Some(who));
        assert_eq!(n.overdue_at, Some(450 + duration));
        assert_eq!(c.pass(460).overdue_at, n.overdue_at);
    }
}
#[test]
fn a_new_head_starts_a_new_deadline_even_when_its_phase_is_identical() {
    // watched.py:91,150,156: same phase on a new revision is new work; closed is unwatched.
    let mut c = Case::new();
    c.ci("pending");
    let n = c.pass(100);
    assert_eq!(n.overdue_at, Some(7300));
    c.push("next-wait-head");
    c.review("reviewer", Verdict::Clean, 200);
    c.ci("pending");
    assert_eq!(c.pass(250).overdue_at, Some(7450));
    assert_eq!(c.pass(260).overdue_at, Some(7450));
    c.hub.edit_pull(REMI, 3, |p| p["state"] = json!("closed"));
    assert_eq!(c.pass(270).overdue_at, None);
}
