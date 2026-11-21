//! `recover-by-kind`: at an expired deadline, a stalled stage is brought
//! back by the way GitHub allows for its kind, once, and a refusal is
//! quoted as GitHub gave it.
use super::flow_pass_case::{Case, REPO};
use super::head;
use crate::flow::*;
use crate::sim::Call;
use serde_json::json;

/// Two hours, the simulated flow's `stall_after` at the PR's level.
const STALL: u64 = 7200;
const START: u64 = 1_000_000;
/// The suite the cancelled run is in, and its workflow run.
const SUITE: u64 = 7;
const RUN: u64 = 9007;

/// A settled PR whose required check's workflow run was cancelled, and
/// nothing replaced it.
fn cancelled(app: &str, run: Option<u64>) -> Case {
    stopped(app, run, "cancelled")
}
/// A settled PR whose required check's workflow run ended `conclusion`.
fn stopped(app: &str, run: Option<u64>, conclusion: &str) -> Case {
    let c = Case::new();
    c.clean_review();
    c.hub.put_checks(
        &head(3),
        json!([{"id":701,"name":"check","status":"completed","conclusion":conclusion,
            "check_suite":{"id":SUITE}}]),
    );
    c.hub.edit_flow(REPO, 3, |s| {
        s.suites.insert(
            SUITE,
            Suite {
                status: "completed".into(),
                conclusion: Some(conclusion.into()),
                app: app.into(),
                workflow: run.map(|run| Workflow {
                    id: "ci".into(),
                    name: "ci".into(),
                    path: Some(".github/workflows/ci.yml".into()),
                    run: Some(run),
                }),
            },
        );
    });
    c
}
fn next(c: &Case, now: u64) -> Next {
    let mut reports = c.pass(now, false);
    assert_eq!(reports.len(), 1);
    reports.remove(0).next.unwrap()
}

/// The `review-settled` status the flow last wrote on the head.
fn status(c: &Case) -> String {
    crate::api::Source::statuses(&c.hub, REPO, &head(3))
        .unwrap()
        .into_iter()
        .find(|s| s.context == SETTLED)
        .and_then(|s| s.description)
        .unwrap()
}
fn asked(c: &Case) -> usize {
    c.hub.count(Call::FlowRerun) + c.hub.count(Call::FlowRerequest) + c.hub.count(Call::FlowUpdate)
}

#[test]
fn a_cancelled_run_nothing_replaced_is_run_again_once() {
    let c = cancelled("github-actions", Some(RUN));
    // Main waits on it for good: "to be run again", by no one.
    let waiting = next(&c, START);
    assert_eq!(waiting.stage, Stage::Ci);
    assert_eq!(
        waiting.description,
        "Settled; waiting for CI: check (cancelled, to be run again)"
    );
    assert_eq!(waiting.overdue_at, Some(START + STALL));
    assert_eq!(next(&c, START + STALL - 1), waiting);
    assert_eq!(asked(&c), 0);
    // At its deadline the flow asks GitHub for the workflow run's rerun.
    let n = next(&c, START + STALL);
    assert_eq!(n.step, Step::Recover(Recovery::Rerun(RUN)));
    assert_eq!(n.overdue_at, Some(START + STALL));
    let recovery = n.recovery.unwrap();
    assert_eq!(recovery.answer, Answer::Accepted);
    assert_eq!(recovery.checks, ["check"]);
    assert_eq!(c.hub.count(Call::FlowRerun), 1);
    // GitHub runs it again, in the same suite: the wait is on that run now.
    let again = next(&c, START + STALL + 60);
    assert_eq!(again.step, Step::Wait { until: None });
    assert_eq!(again.description, "Settled; waiting for CI: check");
    assert_eq!(again.recovery, None);
    // Cancelled again, and overdue again: the rerun isn't asked twice on
    // this head. The stall stays overdue, for whoever takes stalls.
    let mut runs = c.hub.checks_on(&head(3));
    let rerun = runs.pop().unwrap();
    assert!(rerun.id > 701);
    c.hub.put_checks(
        &head(3),
        json!([
            {"id":701,"name":"check","status":"completed","conclusion":"cancelled","check_suite":{"id":SUITE}},
            {"id":rerun.id,"name":"check","status":"completed","conclusion":"cancelled","check_suite":{"id":SUITE}},
        ]),
    );
    let stalled = next(&c, START + STALL + 120);
    let due = stalled.overdue_at.unwrap();
    for now in [due, due + 60, due + 3600] {
        let n = next(&c, now);
        assert_eq!(n.step, Step::Wait { until: None });
        assert_eq!(n.overdue_at, Some(due));
        assert_eq!(
            n.description,
            "Settled; waiting for CI: check (cancelled, to be run again)"
        );
    }
    assert_eq!(asked(&c), 1);
}

#[test]
fn a_refused_rerun_is_quoted_as_github_gave_it_and_never_asked_again() {
    const SAID: &str = "Unable to re-run this workflow run because it was created over a month ago";
    let c = cancelled("github-actions", Some(RUN));
    c.hub.refuse_with(Call::FlowRerun, SAID);
    next(&c, START);
    let n = next(&c, START + STALL);
    assert_eq!(n.step, Step::Recover(Recovery::Rerun(RUN)));
    assert_eq!(
        n.recovery.as_ref().unwrap().answer,
        Answer::Refused(SAID.into())
    );
    // The status says it in the same pass, GitHub's words first.
    assert!(n.description.starts_with(SAID), "{}", n.description);
    assert_eq!(status(&c), n.description);
    assert_eq!(c.hub.count(Call::FlowRerun), 1);
    // Wakes, events and passes after it ask nothing more, and keep quoting
    // it, overdue since the same deadline. Each pass reads its record
    // afresh from disk, as a restart does.
    c.hub.heal(Call::FlowRerun);
    for now in [START + STALL + 60, START + STALL + 3600, START + 3 * STALL] {
        c.hub.label(REPO, 3, &["unrelated"]);
        let after = next(&c, now);
        assert_eq!(after.step, Step::Wait { until: None });
        assert_eq!(after.overdue_at, Some(START + STALL));
        assert_eq!(after.description, n.description);
        assert_eq!(
            after.recovery.as_ref().unwrap().answer,
            Answer::Refused(SAID.into())
        );
        assert_eq!(status(&c), n.description);
    }
    assert_eq!(asked(&c), 1);
}

#[test]
fn a_rerun_github_didnt_answer_is_never_asked_again() {
    // GitHub failing the call, or doing it and the answer lost on the way.
    for lost in [false, true] {
        let c = cancelled("github-actions", Some(RUN));
        if lost {
            c.hub.lose(Call::FlowRerun);
        } else {
            c.hub.fail(Call::FlowRerun);
        }
        next(&c, START);
        let n = next(&c, START + STALL);
        assert!(
            matches!(&n.recovery.as_ref().unwrap().answer, Answer::Unanswered(_)),
            "{n:?}"
        );
        assert!(
            status(&c).starts_with("Overdue; GitHub didn't answer the ask to rerun check"),
            "{}",
            status(&c)
        );
        c.hub.heal(Call::FlowRerun);
        for now in [START + STALL + 600, START + 3 * STALL] {
            assert_eq!(next(&c, now).step, Step::Wait { until: None });
        }
        assert_eq!(c.hub.count(Call::FlowRerun), 1, "lost: {lost}");
    }
}

#[test]
fn another_apps_check_suite_is_rerequested() {
    let c = cancelled("external-ci", None);
    next(&c, START);
    let n = next(&c, START + STALL);
    assert_eq!(n.step, Step::Recover(Recovery::Rerequest(SUITE)));
    assert_eq!(c.hub.count(Call::FlowRerequest), 1);
    assert_eq!(c.hub.count(Call::FlowRerun), 0);
    assert_eq!(
        next(&c, START + STALL + 60).description,
        "Settled; waiting for CI: check"
    );
}

#[test]
fn a_kind_github_wont_rerun_is_never_asked_for() {
    // An Actions run whose workflow run the read didn't name, and a run in
    // a suite no App the read knows: no way GitHub allows is known.
    for (app, workflow) in [("github-actions", None), ("", None)] {
        let c = cancelled(app, workflow);
        let waiting = next(&c, START);
        for now in [START + STALL, START + STALL + 60] {
            let n = next(&c, now);
            assert_eq!(n.step, Step::Wait { until: None });
            assert_eq!(n.description, waiting.description);
            assert_eq!(n.overdue_at, Some(START + STALL));
            assert_eq!(n.recovery, None);
        }
        assert_eq!(asked(&c), 0, "{app:?}");
    }
}

#[test]
fn a_head_behind_its_base_is_updated_first() {
    let c = cancelled("github-actions", Some(RUN));
    c.hub
        .edit_flow(REPO, 3, |s| s.merge_state = "BEHIND".into());
    next(&c, START);
    let n = next(&c, START + STALL);
    assert_eq!(n.step, Step::Recover(Recovery::Update));
    assert_eq!(c.hub.count(Call::FlowUpdate), 1);
    assert_eq!(c.hub.count(Call::FlowRerun), 0);
    // Taken once in the stall: the next pass, GitHub still updating it,
    // asks for nothing else.
    let after = next(&c, START + STALL + 60);
    assert_eq!(after.step, Step::Wait { until: None });
    assert_eq!(asked(&c), 1);
}

#[test]
fn a_cancelled_job_in_a_run_still_going_waits_for_its_run() {
    // GitHub reruns a workflow run once it has finished: until then there's
    // nothing to ask for, and the head's one rerun isn't spent on it.
    let c = cancelled("github-actions", Some(RUN));
    c.hub.edit_flow(REPO, 3, |s| {
        s.suites.get_mut(&SUITE).unwrap().status = "in_progress".into()
    });
    next(&c, START);
    for now in [START + STALL, START + STALL + 60] {
        let n = next(&c, now);
        assert_eq!(n.step, Step::Wait { until: None });
        assert_eq!(n.overdue_at, Some(START + STALL));
    }
    assert_eq!(asked(&c), 0);
    // Once it has, it's asked for at the next stall.
    c.hub.edit_flow(REPO, 3, |s| {
        s.suites.get_mut(&SUITE).unwrap().status = "completed".into()
    });
    let n = next(&c, START + STALL + 120);
    assert_eq!(n.step, Step::Recover(Recovery::Rerun(RUN)));
    assert_eq!(asked(&c), 1);
}

#[test]
fn a_timed_out_run_no_fix_can_take_is_run_again_once() {
    // A timed-out check is red, and goes to a fix; with no fix plugin, the
    // fix stalls, and at its deadline the run is asked for again.
    let mut c = stopped("github-actions", Some(RUN), "timed_out");
    c.config.plugins.fix = None;
    let first = next(&c, START);
    assert_eq!(first.stage, Stage::Fix);
    let n = next(&c, START + 10);
    assert_eq!(n.description, "No plugin configured for fix");
    let due = n.overdue_at.unwrap();
    assert_eq!(next(&c, due - 1).overdue_at, Some(due));
    assert_eq!(asked(&c), 0);
    let n = next(&c, due);
    assert_eq!(n.step, Step::Recover(Recovery::Rerun(RUN)));
    assert_eq!(n.recovery.unwrap().answer, Answer::Accepted);
    assert_eq!(c.hub.count(Call::FlowRerun), 1);
    // Run again, it's pending in CI; timed out again, the fix stalls
    // again, and nothing more is asked on this head.
    assert_eq!(next(&c, due + 60).stage, Stage::Ci);
    let rerun = c.hub.checks_on(&head(3)).pop().unwrap();
    c.hub.put_checks(
        &head(3),
        json!([
            {"id":701,"name":"check","status":"completed","conclusion":"timed_out","check_suite":{"id":SUITE}},
            {"id":rerun.id,"name":"check","status":"completed","conclusion":"timed_out","check_suite":{"id":SUITE}},
        ]),
    );
    next(&c, due + 120);
    let again = next(&c, due + 130).overdue_at.unwrap();
    for now in [again, again + 3600] {
        assert_ne!(next(&c, now).step, Step::Recover(Recovery::Rerun(RUN)));
    }
    assert_eq!(asked(&c), 1);
}
