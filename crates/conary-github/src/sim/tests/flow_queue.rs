//! `waiting-costs-nothing`: a wait on CI starts no plugin and books no wake,
//! and a check's stall clock starts when its run starts, not while it's
//! queued for a runner.
use super::flow_case::Case;
use crate::flow::*;
use serde_json::json;

/// Two hours, the simulated flow's `stall_after` at the PR's level.
const STALL: u64 = 7200;
/// 00:11:40 on the first day of the epoch, when the check is queued.
const QUEUED: u64 = 700;
/// Three hours later, when a runner takes it.
const STARTED: u64 = QUEUED + 3 * 3600;

fn check(c: &Case, status: &str, started_at: &str) {
    c.hub.put_checks(
        &c.scope().head,
        json!([{"id":1,"name":"check","status":status,"conclusion":null,"started_at":started_at}]),
    );
}

#[test]
fn a_queued_check_is_capacity_and_its_clock_starts_with_its_run() {
    let mut c = Case::new();
    check(&c, "queued", "1970-01-01T00:11:40Z");
    let first = c.pass(QUEUED);
    assert_eq!(first.stage, Stage::Ci);
    assert_eq!(first.step, Step::Wait { until: None });
    // Queued past its stall_after, and then some: not overdue, and nothing
    // booked to wake it. The queue time shows as capacity.
    for now in [QUEUED + STALL, STARTED - 1] {
        let n = c.pass(now);
        assert_eq!(n.overdue_at, None, "queued at {now}");
        assert_eq!(n.wait_until, None);
        assert_eq!(n.queued, Some(QUEUED));
        assert_eq!(n, first);
    }
    // A runner takes it: its clock starts then.
    check(&c, "in_progress", "1970-01-01T03:11:40Z");
    let running = c.pass(STARTED + 100);
    assert_eq!(running.step, Step::Wait { until: None });
    assert_eq!(running.overdue_at, Some(STARTED + STALL));
    assert_eq!(running.queued, None);
    // An unrelated event while it runs starts nothing and leaves the wait
    // as it was.
    let wait = c.memory.wait.clone();
    c.label(&["unrelated"]);
    assert_eq!(c.pass(STARTED + 600), running);
    assert_eq!(c.memory.wait, wait);
    // Two hours after its run started, it's overdue.
    assert_eq!(c.pass(STARTED + STALL).overdue_at, Some(STARTED + STALL));
}

#[test]
fn a_started_check_beside_a_queued_one_runs_the_clock_from_its_start() {
    let mut c = Case::new();
    c.hub.put_checks(
        &c.scope().head,
        json!([
            {"id":1,"name":"check","status":"in_progress","conclusion":null,"started_at":"1970-01-01T00:16:40Z"},
            {"id":2,"name":"other","status":"queued","conclusion":null,"started_at":"1970-01-01T00:11:40Z"},
        ]),
    );
    let n = c.pass(QUEUED);
    assert_eq!(n.overdue_at, Some(1000 + STALL));
    assert_eq!(n.queued, None);
}

#[test]
fn a_pending_the_flow_cant_place_keeps_the_waits_own_clock() {
    // A commit status, or a run GitHub gives no start for, may be running:
    // only a run GitHub says is queued stops the clock.
    for status in [
        json!([{"context":"ci/external","state":"pending"}]),
        json!([]),
    ] {
        let mut c = Case::new();
        c.hub.put_checks(
            &c.scope().head,
            json!([
                {"id":1,"name":"check","status":"queued","conclusion":null,"started_at":"1970-01-01T00:11:40Z"},
                {"id":2,"name":"other","status":"in_progress","conclusion":null},
            ]),
        );
        c.hub.put_statuses(&c.scope().head, status);
        let n = c.pass(QUEUED);
        assert_eq!(n.overdue_at, Some(QUEUED + STALL));
        assert_eq!(n.queued, None);
    }
}

#[test]
fn a_finished_job_in_a_run_still_going_has_no_clock_of_its_own() {
    // A workflow's first job finished while its gate queues behind it.
    let s = super::flow_ci_case::Scenario::new();
    let mut run =
        super::flow_ci_case::Run::new(1, &[("check / lane into main", "success")]).going();
    run.checks.push(json!({
        "id": 199, "name": super::flow_ci_case::GATE, "status": "queued",
        "conclusion": null, "started_at": "1970-01-01T00:11:40Z", "check_suite": {"id": 1},
    }));
    let f = s.read(&[run], &[]);
    let (n, _) = next(&f, &s.memory, QUEUED + STALL);
    assert_eq!(n.stage, Stage::Ci);
    assert_eq!(n.overdue_at, None);
    assert_eq!(n.queued, Some(QUEUED));
}

#[test]
fn a_machine_proof_waiting_for_ci_has_no_clock_of_its_own() {
    let mut c = Case::new();
    c.settings
        .proofs
        .insert("proof".into(), Recipe::Job("run proof".into()));
    check(&c, "queued", "1970-01-01T00:11:40Z");
    let n = c.pass(QUEUED + STALL);
    assert_eq!(n.stage, Stage::Ci);
    assert!(n.description.contains("proof"), "{}", n.description);
    assert_eq!(n.overdue_at, None);
    assert_eq!(n.queued, Some(QUEUED));
}

#[test]
fn only_a_pr_no_check_has_reported_for_books_a_wake() {
    let mut c = Case::new();
    c.no_ci();
    assert_eq!(
        c.pass(QUEUED).step,
        Step::Wait {
            until: Some(QUEUED + 300)
        }
    );
    check(&c, "queued", "1970-01-01T00:11:40Z");
    let n = c.pass(QUEUED + 60);
    assert_eq!(n.step, Step::Wait { until: None });
    assert_eq!(n.wait_until, None);
    assert_eq!(n.overdue_at, None);
}
