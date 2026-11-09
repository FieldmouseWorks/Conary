use super::flow_case::Case;
use crate::flow::*;

fn job(c: &Case, kind: Kind, state: JobState) -> (Kind, Job) {
    (
        kind,
        Job {
            id: format!("{kind:?}-1"),
            revision: c.scope(),
            state,
            started: 700,
            repeated: false,
        },
    )
}

#[test]
fn superseded_proofs_and_judges_stop_before_new_work() {
    // superseded.py: proof on A is stopped on B, and its judge is too.
    for kind in [Kind::Proof, Kind::Judge] {
        let mut c = Case::new();
        let (k, j) = job(&c, kind, JobState::Running);
        let id = j.id.clone();
        c.jobs.insert(k, j);
        c.pass(700);
        c.push("new-head");
        c.review("reviewer", Verdict::Clean, 800);
        assert_eq!(c.pass(800).step, Step::StopJob(id.clone()));
        assert_eq!(c.pass(900).step, Step::StopJob(id.clone()));
        assert!(c.memory.strikes.is_empty());
        c.jobs.clear(); // The executor has now observed the stop.
        c.stopped_jobs.insert(id);
        assert_eq!(c.pass(1000).step, Step::Merge);
    }
}

#[test]
fn superseded_ended_jobs_are_left_alone() {
    // superseded.py: one that ended already is not stopped.
    for state in [JobState::Done, JobState::Broken] {
        let mut c = Case::new();
        let (k, j) = job(&c, Kind::Judge, state);
        c.jobs.insert(k, j);
        c.push("next-head");
        c.review("reviewer", Verdict::Clean, 800);
        assert_eq!(c.view(900).step, Step::Merge);
    }
}

#[test]
fn fix_jobs_keep_the_branch_across_pushes() {
    // superseded.py: a fix job pushes to the branch, whatever its head.
    for kind in [
        Kind::Findings,
        Kind::Red,
        Kind::Conflict,
        Kind::JudgeFailure,
        Kind::Description,
    ] {
        let mut c = Case::new();
        let (k, j) = job(&c, kind, JobState::Running);
        c.jobs.insert(k, j);
        c.push("fixes-own-push");
        c.review("reviewer", Verdict::Findings, 800);
        c.threads(1);
        assert!(matches!(c.view(8000).step, Step::Wait { .. }));
        assert_eq!(c.view(8000).stage, Stage::Fix);
    }
}

#[test]
fn description_fix_has_a_distinct_intention_and_rejudges_without_push() {
    // misdescribed.py: edit the description and push nothing; same head judged again.
    let mut c = Case::new();
    c.secret();
    c.judge("misdescribed");
    assert_eq!(c.pass(800).step, Step::HandFix(Kind::Description));
    let head = c.scope().head;
    c.description("It changes the guard's undo snapshot.", 900);
    c.review("reviewer", Verdict::Clean, 901);
    assert_eq!(c.scope().head, head);
    assert_eq!(c.pass(1202).step, Step::Judge);
}

#[test]
fn failed_code_and_description_hold_even_with_pending_ci() {
    // misdescribed.py: either verdict holds whatever CI is still doing.
    for (verdict, kind) in [
        ("failed", Kind::JudgeFailure),
        ("misdescribed", Kind::Description),
    ] {
        let mut c = Case::new();
        c.judge(verdict);
        c.hub.put_checks(
            &c.scope().head,
            serde_json::json!([{"id":1,"name":"check","status":"in_progress","conclusion":null}]),
        );
        assert_eq!(c.view(800).step, Step::HandFix(kind));
        c.ci("success");
        assert_eq!(c.view(900).step, Step::HandFix(kind));
    }
}

#[test]
fn newly_seen_findings_receive_the_full_quiet_period() {
    // handle.py: handoff is measured from max(updatedAt, head first seen).
    let mut c = Case::new();
    c.threads(1);
    c.memory = Memory::default();
    assert_eq!(c.pass(10000).step, Step::Wait { until: Some(11801) });
    assert_eq!(c.pass(11801).step, Step::HandFix(Kind::Findings));
}

#[test]
fn a_returned_head_gets_a_fresh_proof_not_a_stopped_jobs_lost_answer() {
    // superseded.py:52: A -> B -> C -> A never resurrects the job stopped on A.
    let mut c = Case::new();
    c.settings
        .proofs
        .insert("proof".into(), Recipe::Job("run proof".into()));
    let a = c.scope().head;
    let (kind, old) = job(&c, Kind::Proof, JobState::Running);
    let id = old.id.clone();
    c.jobs.insert(kind, old);
    c.pass(700);
    c.push("B");
    c.review("reviewer", Verdict::Clean, 800);
    assert_eq!(c.pass(800).step, Step::StopJob(id.clone()));
    c.stopped_jobs.insert(id.clone());
    c.jobs.get_mut(&Kind::Proof).unwrap().state = JobState::Broken;
    assert_eq!(c.pass(900).step, Step::Prove);
    c.stopped_jobs.clear();
    c.push("C");
    c.push(&a);
    c.review("reviewer", Verdict::Clean, 1000);
    assert_eq!(c.pass(1000).step, Step::Prove);
    assert!(c.memory.retired_jobs.contains(&id));
    assert!(c.memory.strikes.is_empty());
}
#[test]
fn failed_stops_keep_obsolete_work_reserved_until_confirmed_stopped() {
    // superseded.py:67 / unpushed.py:204: a refused stop holds its job, is struck and retried.
    let mut c = Case::new();
    let (kind, old) = job(&c, Kind::Judge, JobState::Running);
    let id = old.id.clone();
    c.jobs.insert(kind, old);
    c.push("new-head");
    assert_eq!(c.pass(800).step, Step::StopJob(id.clone()));
    for (at, due, count) in [(810, 870, 1), (900, 1020, 2), (1100, 1340, 3)] {
        c.failed = Some(Failure::Stop);
        assert_eq!(
            c.pass(at).step,
            Step::Retry {
                kind: Failure::Stop,
                at: due
            }
        );
        assert_eq!(c.memory.strikes[&Failure::Stop], count);
        assert!(c.memory.stopping.contains(&id));
    }
    c.failed = None;
    c.succeeded.insert(Failure::Stop);
    c.stopped_jobs.insert(id.clone());
    c.review("reviewer", Verdict::Clean, 1400);
    assert_eq!(c.pass(1400).step, Step::Merge);
    assert!(c.memory.stopping.is_empty());
    assert!(!c.memory.strikes.contains_key(&Failure::Stop));
}
