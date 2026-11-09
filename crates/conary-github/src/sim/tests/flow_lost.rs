use super::flow_case::Case;
use crate::flow::*;
fn job(c: &Case, id: &str, state: JobState) -> Job {
    Job {
        id: id.into(),
        revision: c.scope(),
        state,
        started: 700,
        repeated: false,
    }
}
#[test]
fn unanswered_jobs_are_counted_from_observations_not_a_callers_retry_flag() {
    // lost.py:69 / 104: two completed turns with no answer, after a second observation each.
    for kind in [
        Kind::Judge,
        Kind::Findings,
        Kind::Red,
        Kind::Proof,
        Kind::JudgeFailure,
        Kind::Description,
    ] {
        let mut c = Case::new();
        match kind {
            Kind::Judge => c.secret(),
            Kind::Findings => c.threads(1),
            Kind::Red => c.ci("failure"),
            Kind::JudgeFailure => c.judge("failed"),
            Kind::Description => c.judge("misdescribed"),
            _ => {
                c.settings
                    .proofs
                    .insert("proof".into(), Recipe::Job("run proof".into()));
            }
        }
        c.jobs.insert(kind, job(&c, "first", JobState::Done));
        assert!(matches!(c.pass(8000).step, Step::Wait { .. }));
        let n = c.pass(8060);
        assert_ne!(n.step, Step::ListOwner);
        c.jobs.insert(kind, job(&c, "second", JobState::Done));
        assert!(matches!(c.pass(8200).step, Step::Wait { .. }));
        assert_eq!(c.pass(8260).step, Step::ListOwner);
    }
}
#[test]
fn job_wakeups_follow_the_original_deadline_then_poll_quarter_hourly() {
    // lost.py:74: jobTimeout plus five minutes, unaffected by replies on the PR.
    let mut c = Case::new();
    c.secret();
    c.jobs
        .insert(Kind::Judge, job(&c, "judge", JobState::Running));
    assert_eq!(c.view(800).step, Step::Wait { until: Some(4600) });
    c.hub
        .edit_flow(crate::sim::REMI, 3, |s| s.updated_at = 4000);
    assert_eq!(c.view(4100).step, Step::Wait { until: Some(4600) });
    assert_eq!(c.view(4900).step, Step::Wait { until: Some(5800) });
}
#[test]
fn broken_jobs_form_one_stall_until_a_turn_finishes() {
    // lost.py:144-198: each crash is one strike; one stall persists through retries and running jobs.
    let mut c = Case::new();
    c.settings
        .proofs
        .insert("proof".into(), Recipe::Job("run proof".into()));
    let mut now = 700;
    let mut since = None;
    for i in 0..4 {
        c.jobs.insert(
            Kind::Proof,
            job(&c, &format!("broken-{i}"), JobState::Broken),
        );
        c.pass(now);
        let n = c.pass(now + 100);
        let due = now + 100 + (1 << i) * 60;
        assert_eq!(
            n.step,
            Step::Retry {
                kind: Failure::Lost,
                at: due
            }
        );
        assert_eq!(c.memory.strikes[&Failure::Lost], i + 1);
        c.pass(due);
        c.jobs.insert(
            Kind::Proof,
            job(&c, &format!("running-{i}"), JobState::Running),
        );
        let n = c.pass(due + 1);
        if i >= 2 {
            let old = *since.get_or_insert(n.overdue_at.unwrap());
            assert_eq!(n.overdue_at, Some(old));
            assert!(!n.on_usage);
        }
        now = due + 100;
    }
    c.jobs
        .insert(Kind::Proof, job(&c, "finished-turn", JobState::Done));
    c.pass(now);
    c.pass(now + 100);
    assert!(!c.memory.strikes.contains_key(&Failure::Lost));
    assert_ne!(c.memory.wait.as_ref().map(|w| w.since), since);
}

#[test]
fn unknown_mergeability_and_unreadable_merge_trees_back_off_and_escalate() {
    // strikes.py:38: mergeable and empty are decision read failures, not an endless uncounted wait.
    for kind in [Failure::Mergeable, Failure::Empty] {
        let mut c = Case::new();
        if kind == Failure::Mergeable {
            c.hub
                .edit_flow(crate::sim::REMI, 3, |s| s.merge_state = "UNKNOWN".into());
        } else {
            c.hub.edit_pull(crate::sim::REMI, 3, |p| {
                p["merge_commit_sha"] = serde_json::json!(super::TRUNK)
            });
        }
        assert_eq!(c.pass(700).step, Step::Retry { kind, at: 760 });
        assert_eq!(c.pass(730).step, Step::Retry { kind, at: 760 });
        assert_eq!(c.pass(760).step, Step::Retry { kind, at: 880 });
        let n = c.pass(880);
        assert_eq!(n.step, Step::Retry { kind, at: 1120 });
        assert_eq!(c.memory.strikes[&kind], 3);
        assert!(n.overdue_at.unwrap() <= 880);
        c.hub
            .edit_flow(crate::sim::REMI, 3, |s| s.merge_state = "CLEAN".into());
        c.hub.edit_pull(crate::sim::REMI, 3, |p| {
            p["merge_commit_sha"] = serde_json::Value::Null
        });
        assert_eq!(c.pass(900).step, Step::Merge);
        assert!(!c.memory.strikes.contains_key(&kind));
    }
}
#[test]
fn a_confirmed_update_request_waits_ten_minutes_for_github_to_push_it() {
    // passes.py / step.py:update_branch: a successful API request does not mean its push is visible yet.
    let mut c = Case::new();
    c.hub
        .edit_flow(crate::sim::REMI, 3, |s| s.merge_state = "BEHIND".into());
    assert_eq!(c.pass(700).step, Step::UpdateBranch);
    c.succeeded.insert(Failure::Update);
    assert_eq!(c.pass(701).step, Step::Wait { until: Some(1301) });
    c.succeeded.clear();
    assert_eq!(c.pass(800).step, Step::Wait { until: Some(1301) });
    assert_eq!(c.pass(1301).step, Step::UpdateBranch);
}

#[test]
fn queued_running_and_unknown_jobs_hold_each_kind_without_starting_another() {
    // handle.py:716 / 778; lost.py:94: queued, running and unknown are live work, not missing answers.
    for state in [JobState::Queued, JobState::Running, JobState::Unknown] {
        for kind in [Kind::Judge, Kind::Proof] {
            let mut c = Case::new();
            if kind == Kind::Judge {
                c.secret();
            } else {
                c.settings
                    .proofs
                    .insert("proof".into(), Recipe::Job("run proof".into()));
            }
            c.jobs.insert(kind, job(&c, "live", state));
            assert_eq!(c.view(800).step, Step::Wait { until: Some(4600) });
        }
    }
}

#[test]
fn a_broken_proof_is_counted_while_its_replacement_waits_for_a_slot() {
    // lost.py:216: reserve the retry's time, but its replacement still waits behind another PR.
    let mut c = Case::new();
    c.settings
        .proofs
        .insert("proof".into(), Recipe::Job("run proof".into()));
    c.settings.proof_slots = Some(1);
    c.jobs
        .insert(Kind::Proof, job(&c, "broken-proof", JobState::Broken));
    assert_eq!(c.pass(1000).step, Step::Wait { until: Some(1060) });
    let other = job(&c, "other-pr-proof", JobState::Running);
    c.hub.pr(crate::sim::REMI, 4, &super::head(4));
    c.proof_data.live.push(ProofHolder {
        number: 4,
        job: other,
        paths: None,
    });
    let n = c.pass(1100);
    assert_eq!(n.stage, Stage::ProofSlot);
    assert_eq!(n.step, Step::Wait { until: Some(1160) });
    assert_eq!(c.memory.strikes[&Failure::Lost], 1);
    assert_eq!(c.pass(1160).stage, Stage::ProofSlot);
    assert_eq!(c.memory.strikes[&Failure::Lost], 1);
    c.proof_data.live.clear();
    assert_eq!(c.pass(1200).step, Step::Prove);
}
