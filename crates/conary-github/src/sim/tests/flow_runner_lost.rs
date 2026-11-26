//! A failed check run whose annotations say only that its runner went away
//! judged nothing: the flow runs it again once, and calls it the machine's,
//! never the change's, if it dies the same way again. Ported from
//! burrow-upkeep's cases for the same rule (burrow#1486).
use super::flow_ci_case::{GATE, Run, Scenario};
use super::flow_pass_case::Case;
use super::head;
use crate::flow::*;
use crate::sim::Call;
use serde_json::json;

const START: u64 = 1_000_000;
const SUITE: u64 = 77;
const RUN: u64 = 9077;
/// What GitHub writes on a job whose hosted runner was shut down under it
/// (run 45213576258, both attempts): the exit, and nothing of its own.
const SHUTDOWN: &str = "Process completed with exit code 143.";
/// What GitHub writes on a job whose hosted runner it lost (job 137975045560).
const LOST: &str = "The hosted runner lost communication with the server. Anything in your \
    workflow that terminates the runner process, starves it for CPU/Memory, or blocks its \
    network access can cause this error.";

fn suite(c: &Case) {
    c.hub.edit_flow(super::flow_pass_case::REPO, 3, |s| {
        s.suites.insert(
            SUITE,
            Suite {
                status: "completed".into(),
                conclusion: Some("failure".into()),
                app: "github-actions".into(),
                workflow: Some(Workflow {
                    id: "ci".into(),
                    name: "ci".into(),
                    path: Some(".github/workflows/ci.yml".into()),
                    run: Some(RUN),
                }),
            },
        );
    });
}

/// A settled PR whose attested `check` passed and whose `build` failed with
/// `annotations` on its first attempt (run 702), or, with `again`, on its
/// second (run 704, after 702).
fn failed(evidence: bool, again: bool, annotations: &[&str]) -> Case {
    let mut c = Case::new();
    if !evidence {
        c.config.evidence = None;
    }
    c.clean_review();
    let mut runs = vec![
        json!({"id": 701, "name": "check", "status": "completed", "conclusion": "success",
               "check_suite": {"id": SUITE}}),
        json!({"id": 702, "name": "build", "status": "completed", "conclusion": "failure",
               "check_suite": {"id": SUITE}}),
    ];
    if again {
        runs.push(json!({"id": 703, "name": "check", "status": "completed",
            "conclusion": "success", "check_suite": {"id": SUITE}}));
        runs.push(json!({"id": 704, "name": "build", "status": "completed",
            "conclusion": "failure", "check_suite": {"id": SUITE}}));
    }
    c.hub.put_checks(&head(3), json!(runs));
    c.attest_run(&head(3), 701, None);
    c.attest_run(&head(3), 703, None);
    c.hub.annotations(702, annotations);
    c.hub.annotations(704, annotations);
    suite(&c);
    c
}

fn next(c: &Case, now: u64) -> Next {
    let mut reports = c.pass(now, false);
    assert_eq!(reports.len(), 1);
    reports.remove(0).next.unwrap()
}

fn no_fix_job(c: &Case) {
    assert!(
        c.record()
            .jobs
            .iter()
            .all(|w| !matches!(w.purpose, jobs::Purpose::Fix(_))),
        "a lost runner is never handed to a fix job: {:?}",
        c.record().jobs
    );
}

#[test]
fn a_lost_runner_reads_as_uncertain_not_red() {
    let s = Scenario::new();
    let run = Run::new(1, &[(GATE, "success"), ("check / build", "failure")]);
    s.hub.annotations(101, &[SHUTDOWN]);
    let f = s.read(&[run], &[]);
    let build = f.checks.iter().find(|c| c.name == "check / build").unwrap();
    assert_eq!(build.state, Ci::Uncertain, "{:?}", f.checks);
    assert_eq!(
        f.lost.iter().map(|r| r.id).collect::<Vec<_>>(),
        [101],
        "{:?}",
        f.checks
    );
}

#[test]
fn a_lost_runner_is_run_again_once_then_named_the_machines() {
    let mut c = failed(true, false, &[SHUTDOWN]);
    let first = next(&c, START);
    assert_eq!(first.stage, Stage::Ci, "{first:?}");
    // An attested repo reruns the whole workflow run, never failed jobs.
    assert_eq!(first.step, Step::Recover(Recovery::Rerun(RUN)), "{first:?}");
    assert_eq!(c.hub.count(Call::FlowRerun), 1);
    assert_eq!(c.hub.count(Call::FlowRerunFailedJobs), 0);
    no_fix_job(&c);

    // While the rerun is queued, the PR waits and asks nothing again.
    let waiting = next(&c, START + 60);
    assert_eq!(waiting.stage, Stage::Ci, "{waiting:?}");
    assert_eq!(waiting.step, Step::Wait { until: None }, "{waiting:?}");
    assert_eq!(c.hub.count(Call::FlowRerun), 1);

    // The rerun's own job died the same way: the machine's, not the change's.
    let queued: Vec<_> = c
        .hub
        .checks_on(&head(3))
        .into_iter()
        .filter(|r| r.status == "queued")
        .collect();
    let check2 = queued.iter().find(|r| r.name == "check").unwrap().id;
    let build2 = queued.iter().find(|r| r.name == "build").unwrap().id;
    c.hub.put_checks(
        &head(3),
        json!([
            {"id": 701, "name": "check", "status": "completed", "conclusion": "success",
             "check_suite": {"id": SUITE}},
            {"id": 702, "name": "build", "status": "completed", "conclusion": "failure",
             "check_suite": {"id": SUITE}},
            {"id": check2, "name": "check", "status": "completed", "conclusion": "success",
             "check_suite": {"id": SUITE}},
            {"id": build2, "name": "build", "status": "completed", "conclusion": "failure",
             "check_suite": {"id": SUITE}},
        ]),
    );
    c.attest_run(&head(3), check2, None);
    c.hub.annotations(build2, &[SHUTDOWN]);
    for now in [START + 120, START + 7200] {
        let escalated = next(&c, now);
        assert_eq!(escalated.stage, Stage::Ci, "at {now}: {escalated:?}");
        assert_eq!(escalated.step, Step::Wait { until: None }, "{escalated:?}");
        assert!(
            escalated.description.contains("attempt 2")
                && escalated.description.contains("base/machine failure")
                && escalated.description.contains("its runner was lost"),
            "{}",
            escalated.description
        );
    }
    assert_eq!(c.hub.count(Call::FlowRerun), 1);
    no_fix_job(&c);

    // With a stuck-role plugin, it goes there once, still no fix job.
    c.config.plugins.job = Some(super::flow_pass_case::example());
    let stuck = next(&c, START + 7300);
    assert!(
        matches!(stuck.step, Step::Stuck(Stuck::Stall, _)),
        "{stuck:?}"
    );
    assert_eq!(c.hub.count(Call::FlowRerun), 1);
    no_fix_job(&c);
}

#[test]
fn a_runner_that_lost_communication_is_run_again_too() {
    let c = failed(true, false, &[LOST]);
    let first = next(&c, START);
    assert_eq!(first.step, Step::Recover(Recovery::Rerun(RUN)), "{first:?}");
    assert_eq!(c.hub.count(Call::FlowRerun), 1);
    no_fix_job(&c);
}

#[test]
fn a_lost_runner_already_rerun_is_the_machines_without_another_ask() {
    let c = failed(true, true, &[SHUTDOWN]);
    let n = next(&c, START);
    assert_eq!(n.stage, Stage::Ci, "{n:?}");
    assert_eq!(n.step, Step::Wait { until: None }, "{n:?}");
    assert!(
        n.description.contains("attempt 2") && n.description.contains("base/machine failure"),
        "{}",
        n.description
    );
    assert_eq!(c.hub.count(Call::FlowRerun), 0);
    assert_eq!(c.hub.count(Call::FlowRerunFailedJobs), 0);
    no_fix_job(&c);
}

#[test]
fn a_lost_runner_in_an_unattested_repo_reruns_only_failed_jobs() {
    let c = failed(false, false, &[SHUTDOWN]);
    let first = next(&c, START);
    assert_eq!(
        first.step,
        Step::Recover(Recovery::RerunFailedJobs(RUN)),
        "{first:?}"
    );
    assert_eq!(c.hub.count(Call::FlowRerunFailedJobs), 1);
    assert_eq!(c.hub.count(Call::FlowRerun), 0);
    no_fix_job(&c);
}

#[test]
fn an_exit_143_beside_the_steps_own_error_is_plain_red() {
    let s = Scenario::new();
    for (k, annotations) in [
        vec![SHUTDOWN, "test negative::rejects_unsigned failed"],
        vec!["Process completed with exit code 1."],
        vec![],
    ]
    .into_iter()
    .enumerate()
    {
        let suite = 1 + k as u64;
        let run = Run::new(suite, &[(GATE, "success"), ("check / build", "failure")]);
        s.hub.annotations(suite * 100 + 1, &annotations);
        s.assert(&[run], Ci::Red, &["check / build"]);
    }
}

#[test]
fn a_real_red_beside_a_lost_runner_is_handed_to_a_fix() {
    let s = Scenario::new();
    let run = Run::new(1, &[(GATE, "failure"), ("check / build", "failure")]);
    s.hub.annotations(101, &[SHUTDOWN]);
    s.assert(&[run], Ci::Red, &[GATE]);
}
