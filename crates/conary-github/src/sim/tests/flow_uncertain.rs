//! Typed uncertain `negative` and gate `error` outcomes and policy-driven
//! single-attempt-safe full workflow rerun recovery (FieldmouseWorks/Conary#602).
use super::flow_pass_case::{Case, REPO, example};
use super::{TRUNK, head};
use crate::api::Source;
use crate::attest::make::{self, Claim, Gate, Log, Negative};
use crate::attest::testing;
use crate::flow::*;
use crate::sim::Call;
use serde_json::json;

const START: u64 = 1_000_000;
const SUITE: u64 = 77;
const RUN: u64 = 9077;

fn install_suite(c: &Case, status: &str, conclusion: Option<&str>) {
    c.hub.edit_flow(REPO, 3, |s| {
        s.suites.insert(
            SUITE,
            Suite {
                status: status.into(),
                conclusion: conclusion.map(str::to_owned),
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

/// A settled PR whose `check` passed attested and whose `negative` failed on
/// attempt 1 without producing any negative attestation (for example, the base
/// check flaked and the negative run couldn't tell).
fn uncertain_unattested_negative() -> Case {
    let c = Case::new();
    c.clean_review();
    c.hub.put_checks(
        &head(3),
        json!([
            {"id": 701, "name": "check", "status": "completed", "conclusion": "success",
             "check_suite": {"id": SUITE}},
            {"id": 702, "name": "negative", "status": "completed", "conclusion": "failure",
             "check_suite": {"id": SUITE}},
        ]),
    );
    c.attest_run(&head(3), 701, None);
    install_suite(&c, "completed", Some("failure"));
    c
}

fn attest_negative_error(c: &Case, run_id: u64, why: &str) {
    let base = Source::merge_base(&c.hub, REPO, TRUNK, &head(3)).unwrap();
    let stmt = make::statement(&Claim::Negative {
        base,
        head: head(3),
        result: Negative::Error(why.into()),
        tree: None,
        tests: vec!["tests/new.rs".into()],
        checks: vec!["crates".into()],
        skipped: vec![],
        unrun: vec![],
        log: Log::of(b"base check flaked\n", "negative-log-1"),
    })
    .unwrap()
    .text();
    let claims = testing::claims(REPO, &head(3), run_id, &stmt);
    c.hub
        .attest(run_id, &testing::file(&stmt, &testing::token(&claims)));
}

fn attest_gate(c: &Case, run_id: u64, result: Gate) {
    let base = Source::merge_base(&c.hub, REPO, TRUNK, &head(3)).unwrap();
    let tree = crate::branches::Branches::tree(&c.hub, REPO, &head(3)).unwrap();
    let stmt = make::statement(&Claim::Revision {
        base,
        head: head(3),
        result,
        tree,
        skipped: vec![],
        log: Log::of(b"gate log\n", "gate-log-1"),
    })
    .unwrap()
    .text();
    let claims = testing::claims(REPO, &head(3), run_id, &stmt);
    c.hub
        .attest(run_id, &testing::file(&stmt, &testing::token(&claims)));
}

fn next(c: &Case, now: u64) -> Next {
    let mut reports = c.pass(now, false);
    assert_eq!(reports.len(), 1);
    reports.remove(0).next.unwrap()
}

#[test]
fn uncertain_unattested_negative_triggers_one_full_workflow_rerun_not_a_fix_job() {
    let c = uncertain_unattested_negative();
    let n = next(&c, START);
    assert_eq!(n.stage, Stage::Ci, "unexpected stage: {n:?}");
    assert_eq!(
        n.step,
        Step::Recover(Recovery::Rerun(RUN)),
        "uncertain negative must recover via full workflow rerun, not dispatch a PR fix job"
    );
    assert_eq!(n.recovery.as_ref().unwrap().answer, Answer::Accepted);
    assert_eq!(c.hub.count(Call::FlowRerun), 1);
    assert_eq!(c.hub.count(Call::FlowRerunFailedJobs), 0);
    assert!(
        c.record().jobs.is_empty(),
        "must not dispatch a PR fix job for an uncertain negative run"
    );

    // Full workflow rerun re-queued BOTH `check` and `negative` cohesively on attempt 2.
    let queued: Vec<_> = c
        .hub
        .checks_on(&head(3))
        .into_iter()
        .filter(|r| r.status == "queued")
        .collect();
    let mut names: Vec<_> = queued.iter().map(|r| r.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["check", "negative"]);

    // While attempt 2 runs, the PR waits in CI and never reruns again.
    let waiting = next(&c, START + 60);
    assert_eq!(waiting.stage, Stage::Ci);
    assert_eq!(waiting.step, Step::Wait { until: None });
    assert_eq!(c.hub.count(Call::FlowRerun), 1);

    // Once attempt 2 passes with cohesive attestations, the PR lands cleanly.
    c.hub.finish_again(&head(3), "success");
    for run in queued {
        if run.name == "check" {
            c.attest_run(&head(3), run.id, None);
        } else {
            let base = Source::merge_base(&c.hub, REPO, TRUNK, &head(3)).unwrap();
            let stmt = testing::negative_statement(&base, &head(3), "fail");
            let claims = testing::claims(REPO, &head(3), run.id, &stmt);
            c.hub
                .attest(run.id, &testing::file(&stmt, &testing::token(&claims)));
        }
    }
    let landed = next(&c, START + 120);
    assert_eq!(landed.step, Step::Merge);
    assert_eq!(c.hub.count(Call::Land), 1);
}

#[test]
fn attested_negative_error_triggers_one_full_workflow_rerun_not_a_fix_job() {
    let c = uncertain_unattested_negative();
    attest_negative_error(
        &c,
        702,
        "the same check on the base, fails too, so it can't tell",
    );
    let n = next(&c, START);
    assert_eq!(n.stage, Stage::Ci);
    assert_eq!(n.step, Step::Recover(Recovery::Rerun(RUN)));
    assert_eq!(c.hub.count(Call::FlowRerun), 1);
    assert_eq!(c.hub.count(Call::FlowRerunFailedJobs), 0);
    assert!(c.record().jobs.is_empty());
}

#[test]
fn second_uncertain_attempt_stops_retrying_and_escalates_without_looping() {
    let mut c = uncertain_unattested_negative();
    // Attempt 1 triggers one full workflow rerun.
    let first = next(&c, START);
    assert_eq!(first.step, Step::Recover(Recovery::Rerun(RUN)));
    assert_eq!(c.hub.count(Call::FlowRerun), 1);

    // Attempt 2 completes with `check` passing and `negative` failing uncertainly again.
    let queued: Vec<_> = c
        .hub
        .checks_on(&head(3))
        .into_iter()
        .filter(|r| r.status == "queued")
        .collect();
    let check2 = queued.iter().find(|r| r.name == "check").unwrap().id;
    let neg2 = queued.iter().find(|r| r.name == "negative").unwrap().id;
    c.hub.put_checks(
        &head(3),
        json!([
            {"id": 701, "name": "check", "status": "completed", "conclusion": "success",
             "check_suite": {"id": SUITE}},
            {"id": 702, "name": "negative", "status": "completed", "conclusion": "failure",
             "check_suite": {"id": SUITE}},
            {"id": check2, "name": "check", "status": "completed", "conclusion": "success",
             "check_suite": {"id": SUITE}},
            {"id": neg2, "name": "negative", "status": "completed", "conclusion": "failure",
             "check_suite": {"id": SUITE}},
        ]),
    );
    c.attest_run(&head(3), check2, None);
    attest_negative_error(&c, neg2, "base check flaked again");

    // Without a stuck-role plugin: stops retrying, escalates immediately as overdue
    // base/machine failure at Stage::Ci, never dispatches a PR fix job.
    for now in [START + 120, START + 600, START + 7200] {
        let escalated = next(&c, now);
        assert_eq!(escalated.stage, Stage::Ci, "at {now}: {escalated:?}");
        assert_eq!(escalated.step, Step::Wait { until: None });
        assert_eq!(escalated.overdue_at, Some(START + 120));
        assert!(
            escalated.description.contains("attempt 2")
                && escalated.description.contains("base/machine failure"),
            "{}",
            escalated.description
        );
    }
    assert_eq!(c.hub.count(Call::FlowRerun), 1);
    assert_eq!(c.hub.count(Call::FlowRerunFailedJobs), 0);
    assert!(c.record().jobs.is_empty());

    // With a stuck-role plugin configured: escalates once via Step::Stuck(Stall)
    // instead of looping or starting a PR fix job.
    c.config.plugins.job = Some(example());
    let stuck = next(&c, START + 7300);
    assert!(
        matches!(stuck.step, Step::Stuck(Stuck::Stall, _)),
        "expected stuck-role escalation, got {stuck:?}"
    );
    assert_eq!(c.hub.count(Call::FlowRerun), 1);
    assert!(
        c.record()
            .jobs
            .iter()
            .all(|w| !matches!(w.purpose, jobs::Purpose::Fix(_))),
        "must never dispatch a PR fix job"
    );
}

#[test]
fn external_second_attempt_bounded_by_run_attempt_never_reruns_again() {
    // Even with fresh local state (e.g. after a restart or human rerun),
    // run_attempt >= 2 stops retrying immediately.
    let c = Case::new();
    c.clean_review();
    c.hub.put_checks(
        &head(3),
        json!([
            {"id": 701, "name": "check", "status": "completed", "conclusion": "success",
             "check_suite": {"id": SUITE}},
            {"id": 702, "name": "negative", "status": "completed", "conclusion": "failure",
             "check_suite": {"id": SUITE}},
            {"id": 703, "name": "check", "status": "completed", "conclusion": "success",
             "check_suite": {"id": SUITE}},
            {"id": 704, "name": "negative", "status": "completed", "conclusion": "failure",
             "check_suite": {"id": SUITE}},
        ]),
    );
    c.attest_run(&head(3), 703, None);
    install_suite(&c, "completed", Some("failure"));

    let n = next(&c, START);
    assert_eq!(n.stage, Stage::Ci);
    assert_eq!(n.step, Step::Wait { until: None });
    assert_eq!(n.overdue_at, Some(START));
    assert_eq!(c.hub.count(Call::FlowRerun), 0);
    assert_eq!(c.hub.count(Call::FlowRerunFailedJobs), 0);
    assert!(c.record().jobs.is_empty());
}

#[test]
fn single_attempt_attested_repos_are_structurally_prevented_from_partial_rerun_failed_jobs() {
    let c = uncertain_unattested_negative();
    // Policy derived from evidence forbids partial rerun-failed-jobs.
    let policy = RerunPolicy::for_evidence(c.config.evidence.as_ref());
    assert_eq!(policy, RerunPolicy::FullWorkflowOnly);
    assert_eq!(policy.workflow_recovery(RUN), Recovery::Rerun(RUN));
    assert_eq!(
        policy.apply(Recovery::RerunFailedJobs(RUN)),
        Recovery::Rerun(RUN)
    );
    assert!(!policy.permits(&Recovery::RerunFailedJobs(RUN)));
    assert!(policy.permits(&Recovery::Rerun(RUN)));

    // Unattested repo policy allows partial rerun-failed-jobs.
    let unattested = RerunPolicy::for_evidence(None);
    assert_eq!(unattested, RerunPolicy::FailedJobsAllowed);
    assert_eq!(
        unattested.workflow_recovery(RUN),
        Recovery::RerunFailedJobs(RUN)
    );
    assert!(unattested.permits(&Recovery::RerunFailedJobs(RUN)));

    // Unattested repo recovering a cancelled check uses rerun-failed-jobs.
    let mut no_evidence = Case::new();
    no_evidence.config.evidence = None;
    no_evidence.clean_review();
    no_evidence.hub.put_checks(
        &head(3),
        json!([
            {"id": 701, "name": "check", "status": "completed", "conclusion": "success",
             "check_suite": {"id": SUITE}},
            {"id": 702, "name": "other", "status": "completed", "conclusion": "cancelled",
             "check_suite": {"id": SUITE}},
        ]),
    );
    install_suite(&no_evidence, "completed", Some("cancelled"));
    next(&no_evidence, START);
    let recovered = next(&no_evidence, START + 7200);
    assert_eq!(
        recovered.step,
        Step::Recover(Recovery::RerunFailedJobs(RUN))
    );
    assert_eq!(no_evidence.hub.count(Call::FlowRerunFailedJobs), 1);
    assert_eq!(no_evidence.hub.count(Call::FlowRerun), 0);

    // Defense in depth: even if Step::Recover(RerunFailedJobs) is constructed
    // directly for an attested repo, execute::act refuses it and never calls
    // rerun-failed-jobs on GitHub.
    let repo: conary_core::op::Repo = REPO.parse().unwrap();
    let mut facts = read(
        &c.hub,
        &repo,
        3,
        &c.policy,
        c.config.settings.clone(),
        &c.config.trusted_authors,
    )
    .unwrap();
    crate::flow::hydrate_uncertain_for_test(&c.hub, &mut facts, c.config.evidence.as_ref());
    let mut record = Record::default();
    let data = Data::open(c.dir.path(), &repo).unwrap();
    let pass = Pass {
        config: &c.config,
        policy: &c.policy,
        root: c.dir.path(),
        launcher: &c.deferred,
        now: START,
        dry_run: false,
        ledger: None,
    };
    let forbidden = Next {
        stage: Stage::Ci,
        step: Step::Recover(Recovery::RerunFailedJobs(RUN)),
        description: "forced partial rerun".into(),
        wait_until: None,
        overdue_at: None,
        queued: None,
        proof: None,
        deadline: None,
        wait_on: None,
        on_usage: false,
        owner_notice: None,
        switches: vec![],
        priority: Level::Medium,
        degraded: vec![],
        recovery: Some(Recovered {
            head: head(3),
            since: START,
            recovery: Recovery::RerunFailedJobs(RUN),
            checks: vec!["negative".into()],
            answer: Answer::Asked,
            immediate: true,
        }),
        owner: None,
    };
    let mut store = c.store.borrow_mut();
    let err = crate::flow::execute_for_test(
        &pass,
        &c.hub,
        &facts,
        &forbidden,
        &mut record,
        &data,
        Some(&mut store),
    )
    .unwrap_err();
    assert!(
        err.1.to_string().contains("single-attempt attestation"),
        "{}",
        err.1
    );
    assert_eq!(c.hub.count(Call::FlowRerunFailedJobs), 0);
}

#[test]
fn attested_negative_pass_is_a_pr_failure_and_dispatches_fix_job() {
    // When negative attests `pass` (the new tests passed on old code), that IS
    // a PR-code defect: it must go to Stage::Fix and dispatch a fix job, never rerun.
    let c = uncertain_unattested_negative();
    let base = Source::merge_base(&c.hub, REPO, TRUNK, &head(3)).unwrap();
    let stmt = testing::negative_statement(&base, &head(3), "pass");
    let claims = testing::claims(REPO, &head(3), 702, &stmt);
    c.hub
        .attest(702, &testing::file(&stmt, &testing::token(&claims)));

    let first = next(&c, START);
    assert_eq!(first.stage, Stage::Fix);
    let handed = next(&c, START + 1);
    assert_eq!(handed.stage, Stage::Fix);
    assert_eq!(handed.step, Step::HandFix(Kind::Red));
    assert_eq!(c.hub.count(Call::FlowRerun), 0);
    assert_eq!(c.hub.count(Call::FlowRerunFailedJobs), 0);
    assert_eq!(c.record().jobs.len(), 1);
}

#[test]
fn attested_required_gate_error_triggers_one_full_rerun_while_gate_fail_fixes() {
    // Attested gate `error` -> one full workflow rerun, not a PR fix job.
    let c = Case::new();
    c.clean_review();
    c.hub.put_checks(
        &head(3),
        json!([
            {"id": 701, "name": "check", "status": "completed", "conclusion": "failure",
             "check_suite": {"id": SUITE}},
            {"id": 702, "name": "negative", "status": "completed", "conclusion": "success",
             "check_suite": {"id": SUITE}},
        ]),
    );
    attest_gate(&c, 701, Gate::Error("runner disk full".into()));
    let base = Source::merge_base(&c.hub, REPO, TRUNK, &head(3)).unwrap();
    let neg_stmt = testing::negative_statement(&base, &head(3), "none");
    let neg_claims = testing::claims(REPO, &head(3), 702, &neg_stmt);
    c.hub
        .attest(702, &testing::file(&neg_stmt, &testing::token(&neg_claims)));
    install_suite(&c, "completed", Some("failure"));

    let n = next(&c, START);
    assert_eq!(n.stage, Stage::Ci);
    assert_eq!(n.step, Step::Recover(Recovery::Rerun(RUN)));
    assert_eq!(c.hub.count(Call::FlowRerun), 1);
    assert_eq!(c.hub.count(Call::FlowRerunFailedJobs), 0);
    assert!(c.record().jobs.is_empty());

    // Attested gate `fail` -> genuine PR failure -> dispatches PR fix job.
    let c_fail = Case::new();
    c_fail.clean_review();
    c_fail.hub.put_checks(
        &head(3),
        json!([
            {"id": 701, "name": "check", "status": "completed", "conclusion": "failure",
             "check_suite": {"id": SUITE}},
            {"id": 702, "name": "negative", "status": "completed", "conclusion": "success",
             "check_suite": {"id": SUITE}},
        ]),
    );
    attest_gate(&c_fail, 701, Gate::Fail);
    install_suite(&c_fail, "completed", Some("failure"));
    next(&c_fail, START);
    let handed = next(&c_fail, START + 1);
    assert_eq!(handed.step, Step::HandFix(Kind::Red));
    assert_eq!(c_fail.hub.count(Call::FlowRerun), 0);
}

#[test]
fn uncertain_negative_waits_for_running_suite_and_degrades_on_api_error() {
    // Still running suite: wait without spending the rerun or dispatching fix.
    let c = uncertain_unattested_negative();
    install_suite(&c, "in_progress", None);
    let waiting = next(&c, START);
    assert_eq!(waiting.stage, Stage::Ci);
    assert_eq!(waiting.step, Step::Wait { until: None });
    assert_eq!(c.hub.count(Call::FlowRerun), 0);
    assert!(c.record().jobs.is_empty());
    // Once the suite finishes, attempt 1 recovers immediately.
    install_suite(&c, "completed", Some("failure"));
    let recovered = next(&c, START + 60);
    assert_eq!(recovered.step, Step::Recover(Recovery::Rerun(RUN)));
    assert_eq!(c.hub.count(Call::FlowRerun), 1);

    // Transient API failure inspecting attestation degrades safely without guessing.
    let c_err = uncertain_unattested_negative();
    c_err.hub.fail(Call::Artifacts);
    let degraded = next(&c_err, START);
    assert_eq!(degraded.stage, Stage::Ci);
    assert_eq!(degraded.step, Step::Wait { until: None });
    assert!(
        degraded
            .degraded
            .iter()
            .any(|d| matches!(&d.what, Degraded::Lookup { check } if check == "negative")),
        "{degraded:?}"
    );
    assert_eq!(c_err.hub.count(Call::FlowRerun), 0);
    assert!(c_err.record().jobs.is_empty());
}
