//! A pause and a hold vacate: their PRs' workers stop at once, through the
//! stop path, their proof slots free as the workers end, and nothing new
//! starts. Nothing a worker finished is redone once they're lifted.
use super::flow_pass_case::{Case, REPO};
use super::flow_pass_slots::{WAITING, prepared, step, two, work};
use crate::flow::jobs::Purpose;
use crate::flow::*;

fn repo() -> conary_core::op::Repo {
    REPO.parse().unwrap()
}
fn pause(c: &Case, reason: &str) {
    let paused = Pause {
        scope: Scope::Repository,
        reason: reason.into(),
        at: 0,
    };
    crate::flow::pause::pause(c.dir.path(), &repo(), &paused).unwrap();
}
fn record(c: &Case, pr: u64) -> Record {
    Data::read(c.dir.path(), &repo(), pr).unwrap()
}
fn proofs(c: &Case, pr: u64) -> Vec<crate::flow::jobs::Work> {
    work(c, pr, |p| matches!(p, Purpose::Proof))
}
fn marker(work: &crate::flow::jobs::Work) -> Vec<u8> {
    std::fs::read(work.path.with_extension("cancel")).unwrap()
}
/// What a stopped worker leaves, without the strikes, retries or lost jobs
/// of a failure.
fn no_failure(c: &Case, pr: u64) {
    let record = record(c, pr);
    assert!(
        record.memory.strikes.is_empty(),
        "{:?}",
        record.memory.strikes
    );
    assert_eq!(record.memory.retry, None);
    assert!(record.memory.lost_jobs.is_empty());
    assert!(record.memory.broken_jobs.is_empty());
    assert_eq!(record.failed, None);
}

/// One proof slot: PR 3's proof holds it and PR 5's waits. Holding PR 3
/// stops its proof, and once that worker ends PR 5's proof takes the slot.
/// Before holds vacated, PR 3 waited "On hold" and kept the slot.
#[test]
fn holding_a_proof_frees_its_slot_for_the_next() {
    let c = two(1);
    let now = crate::flow::jobs::now();
    prepared(&c, now, true);
    let reports = c.pass(now + 1, false);
    assert_eq!(step(&reports, 0).1, Step::Prove);
    assert_eq!(step(&reports, 1), WAITING);
    let held = proofs(&c, 3).remove(0);

    c.hub.label(REPO, 3, &["hold"]);
    let reports = c.pass(now + 2, false);
    assert_eq!(
        step(&reports, 0),
        (Stage::None, Step::StopJob(held.job.id.clone()))
    );
    assert_eq!(marker(&held), b"held\n");
    // The slot is the worker's until it ends.
    assert_eq!(step(&reports, 1), WAITING);

    c.run_worker();
    let reports = c.pass(now + 3, false);
    assert_eq!(
        reports[0].next.as_ref().unwrap().description,
        "On hold (the hold label)"
    );
    assert_eq!(step(&reports, 1), (Stage::Proof, Step::Prove));
    assert_eq!(proofs(&c, 5).len(), 1);
    assert_eq!(c.deferred.0.borrow().len(), 1);
    no_failure(&c, 3);
    assert!(record(&c, 3).memory.retired_jobs.contains(&held.job.id));
}

/// Pausing the repository, written as `cn flow pause` writes it with no
/// daemon running, stops every worker at once and starts none. Resuming
/// carries on from what's saved: the proofs start again from their requests,
/// and nothing else is redone, before or after they answer.
#[test]
fn a_pause_stops_every_worker_and_a_resume_redoes_nothing() {
    let c = two(2);
    let now = crate::flow::jobs::now();
    prepared(&c, now, true);
    c.pass(now + 1, false);
    let running: Vec<_> = [3, 5].map(|pr| proofs(&c, pr).remove(0)).into();
    assert_eq!(c.deferred.0.borrow().len(), 2);
    let before = |c: &Case| [3, 5].map(|pr| work(c, pr, |_| true).len());
    let started = before(&c);

    pause(&c, "the machine is moving");
    let reports = c.pass(now + 2, false);
    for (at, proof) in running.iter().enumerate() {
        assert_eq!(
            step(&reports, at),
            (Stage::None, Step::StopJob(proof.job.id.clone()))
        );
        assert_eq!(marker(proof), b"paused\n");
    }
    c.run_worker();
    for at in [now + 3, now + 4] {
        for report in c.pass(at, false) {
            let next = report.next.unwrap();
            assert_eq!(next.step, Step::Wait { until: None });
            assert_eq!(next.stage, Stage::None);
            assert_eq!(next.description, "Paused: the machine is moving");
            assert_eq!(next.overdue_at, None);
            assert_eq!(report.error, None);
        }
    }
    assert!(c.deferred.0.borrow().is_empty());
    assert_eq!(before(&c), started);
    for pr in [3, 5] {
        no_failure(&c, pr);
    }

    crate::flow::pause::resume(c.dir.path(), &repo()).unwrap();
    let reports = c.pass(now + 5, false);
    for at in [0, 1] {
        assert_eq!(step(&reports, at), (Stage::Proof, Step::Prove));
    }
    // Only the two proofs started again: no preparation, review or fix.
    let after = before(&c);
    assert_eq!(after, started.map(|n| n + 1));
    for pr in [3, 5] {
        assert_eq!(proofs(&c, pr).len(), 2);
    }
    // The proofs answer, and what's left is their producer's evidence: no
    // review, preparation or fix round comes back.
    c.run_worker();
    for report in c.pass(now + 6, false) {
        let next = report.next.unwrap();
        assert_eq!(next.stage, Stage::Proof, "{next:?}");
        assert_eq!(next.step, Step::Wait { until: None });
    }
    assert_eq!(before(&c), after);
}

/// The pause is read before each PR's decision, not once a pass: a pause
/// written while one PR's work starts keeps the next PR's from starting.
#[test]
fn a_pause_written_during_a_pass_stops_the_prs_after_it() {
    struct Pausing<'a>(&'a Case);
    impl crate::flow::jobs::Launch for Pausing<'_> {
        fn launch(&self, path: &std::path::Path) -> crate::flow::persist::Result<()> {
            pause(self.0, "paused mid-pass");
            self.0.deferred.launch(path)
        }
    }
    let c = two(2);
    let now = crate::flow::jobs::now();
    let reports = Pass {
        config: &c.config,
        policy: &c.policy,
        root: c.dir.path(),
        launcher: &Pausing(&c),
        now,
        dry_run: false,
        ledger: None,
    }
    .run(&c.hub, &repo(), None, Some(&mut c.store.borrow_mut()))
    .unwrap();
    assert_eq!(step(&reports, 0), (Stage::Proof, Step::Prepare));
    assert_eq!(
        reports[1].next.as_ref().unwrap().description,
        "Paused: paused mid-pass"
    );
    assert_eq!(c.deferred.0.borrow().len(), 1);
    assert!(work(&c, 5, |_| true).is_empty());
}

/// A pause that can't be read stops the pass's decisions, never reads as no
/// pause.
#[test]
fn an_unreadable_pause_is_no_permission_to_work() {
    let c = two(2);
    let dir = crate::flow::persist::dir(c.dir.path(), &repo());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("pause"), b"{\"scope\":\"everything\"}").unwrap();
    for report in c.pass(crate::flow::jobs::now(), false) {
        assert!(report.error.is_some(), "{report:?}");
        assert_eq!(report.next, None);
    }
    assert!(c.deferred.0.borrow().is_empty());
}

/// A pause written after a decision, while its effect is under way, still
/// keeps the effect off: a merge isn't sent and a plugin isn't started, and
/// neither is a failure.
#[test]
fn a_pause_written_after_the_decision_keeps_its_effect_off() {
    use crate::sim::Call;
    let paused = |c: &Case| {
        let root = c.dir.path().to_path_buf();
        move |_: &crate::sim::Hub| {
            let paused = Pause {
                scope: Scope::Repository,
                reason: "after the decision".into(),
                at: 0,
            };
            crate::flow::pause::pause(&root, &repo(), &paused).unwrap();
        }
    };
    for call in [Call::FlowStatus, Call::FlowStage] {
        let c = Case::new();
        c.clean_review();
        c.hub.race(call, paused(&c));
        let reports = c.pass(crate::flow::jobs::now(), false);
        assert_eq!(step(&reports, 0).1, Step::Merge, "{call:?}");
        assert!(
            reports[0].error.as_deref().unwrap().contains("paused"),
            "{call:?}"
        );
        assert_eq!(c.hub.count(Call::Land), 0, "{call:?}");
        assert!(record(&c, 3).landing.is_none(), "{call:?}");
        no_failure(&c, 3);
    }
    let c = two(1);
    c.hub.race(Call::FlowStage, paused(&c));
    let reports = c.pass(crate::flow::jobs::now(), false);
    assert_eq!(step(&reports, 0), (Stage::Proof, Step::Prepare));
    assert!(reports[0].error.as_deref().unwrap().contains("paused"));
    assert!(c.deferred.0.borrow().is_empty());
    assert!(work(&c, 3, |_| true).is_empty());
    no_failure(&c, 3);
}

fn grant_races(mode: &str) {
    use crate::api::Source;
    use crate::sim::{Call, On};
    use crate::train::Mark;
    for on in [
        On::from(Call::FlowStatus).mark(Mark::Waiting),
        On::from(Call::FlowStage),
        On::from(Call::FlowStatus).mark(Mark::Landing),
    ] {
        let mut c = Case::new();
        c.clean_review();
        match mode {
            "delegated" => c.as_bot(),
            "join" | "seated" => c.config.settings.train = true,
            _ => {}
        }
        let now = crate::flow::jobs::now();
        if mode == "seated" {
            let reports = c.pass(now, false);
            assert_eq!(reports[0].error, None, "{reports:?}");
            assert_eq!(step(&reports, 0).1, Step::JoinTrain);
        }
        let granted_before = c
            .hub
            .statuses(REPO, &super::head(3))
            .unwrap()
            .iter()
            .filter(|s| s.context == SETTLED && s.state == "success")
            .count();
        let root = c.dir.path().to_path_buf();
        let write_pause = move |_: &crate::sim::Hub| {
            crate::flow::pause::pause(
                &root,
                &repo(),
                &Pause {
                    scope: Scope::Repository,
                    reason: "permission race".into(),
                    at: 0,
                },
            )
            .unwrap();
        };
        if mode == "seated" && on == On::from(Call::FlowStatus).mark(Mark::Waiting) {
            // Recovery first withdraws the previous pass's permission.
            // Pause on the executor's next write, after its new decision.
            let again = on.clone();
            c.hub
                .race(on.clone(), move |hub| hub.race(again, write_pause));
        } else {
            c.hub.race(on.clone(), write_pause);
        }
        let reports = c.pass(now + 1, false);
        let expected = match mode {
            "delegated" => Step::Settle,
            "join" => Step::JoinTrain,
            "seated" => Step::Wait { until: None },
            _ => Step::Merge,
        };
        assert_eq!(step(&reports, 0).1, expected, "{mode} {on:?}");
        let statuses = c.hub.statuses(REPO, &super::head(3)).unwrap();
        assert_eq!(
            statuses
                .iter()
                .find(|s| s.context == SETTLED)
                .unwrap()
                .state,
            "pending",
            "{mode} {on:?}"
        );
        if on != On::from(Call::FlowStatus).mark(Mark::Landing) {
            assert_eq!(
                statuses
                    .iter()
                    .filter(|s| s.context == SETTLED && s.state == "success")
                    .count(),
                granted_before,
                "permission published after pause: {mode} {on:?}"
            );
        }
        assert!(record(&c, 3).live.is_empty(), "{mode} {on:?}");
        assert_eq!(c.hub.count(Call::Land), 0, "{mode} {on:?}");
        assert!(
            reports[0]
                .error
                .as_deref()
                .is_some_and(|e| e.contains("paused")),
            "{mode} {on:?}: {reports:?}"
        );
        if mode != "seated" {
            assert!(
                c.store
                    .borrow()
                    .ledger()
                    .train(&repo())
                    .is_none_or(|t| t.joined().is_empty())
            );
        }
        no_failure(&c, 3);
    }
}
#[test]
fn pause_blocks_direct_permission_races() {
    grant_races("direct");
}
#[test]
fn pause_blocks_delegated_permission_races() {
    grant_races("delegated");
}
#[test]
fn pause_blocks_train_admission_permission_races() {
    grant_races("join");
}
#[test]
fn pause_blocks_seated_permission_renewal_races() {
    grant_races("seated");
}

/// Own real invocations of every kind, as overlapping workers on successive
/// revisions can leave behind. Cancellation must not depend on which kind
/// the decision would choose next.
fn workers(c: &Case, now: u64) -> Vec<crate::flow::jobs::Work> {
    start_workers(c, now, 0..4)
}
/// Start `which` of the workers, in the order fix, judge, prepare, proof.
fn start_workers(
    c: &Case,
    now: u64,
    which: std::ops::Range<usize>,
) -> Vec<crate::flow::jobs::Work> {
    use conary_core::plugin::{Identity, Task};
    let facts = read(
        &c.hub,
        &repo(),
        3,
        &c.policy,
        c.config.settings.clone(),
        &c.config.trusted_authors,
    )
    .unwrap();
    let data = Data::open(c.dir.path(), &repo()).unwrap();
    let mut record = data.load(3).unwrap();
    record.memory.head = facts.revision.head.clone();
    let author = Identity {
        engine: "fixture".into(),
        session: None,
    };
    let tasks = [
        (
            Purpose::Fix(Kind::Findings),
            Task::Fix {
                repo: REPO.into(),
                pr: 3,
                head: facts.revision.head.clone(),
                problems: vec![],
            },
        ),
        (
            Purpose::Judge,
            Task::Judge {
                repo: REPO.into(),
                pr: 3,
                head: facts.revision.head.clone(),
                against: "intent".into(),
                author,
                reviewers: vec![],
                why: vec![],
                context: String::new(),
            },
        ),
        (
            Purpose::Prepare,
            Task::Prepare {
                repo: REPO.into(),
                head: facts.revision.head.clone(),
                tree: super::TRUNK.into(),
                checks: vec![],
            },
        ),
        (
            Purpose::Proof,
            Task::Proof {
                repo: REPO.into(),
                head: facts.revision.head.clone(),
                tree: super::TRUNK.into(),
                checks: vec![],
            },
        ),
    ];
    let pass = Pass {
        config: &c.config,
        policy: &c.policy,
        root: c.dir.path(),
        launcher: &c.deferred,
        now,
        dry_run: false,
        ledger: None,
    };
    for (purpose, task) in tasks.into_iter().skip(which.start).take(which.len()) {
        pass.start(&facts, purpose, task, vec![], &mut record, &data)
            .unwrap();
    }
    record.jobs
}
fn worker_case() -> Case {
    let mut c = Case::new();
    c.config.plugins.judge = c.config.plugins.fix.clone();
    c.config.plugins.proof = c.config.plugins.fix.clone();
    c.clean_review();
    c
}
fn cleanup_case(cleanup: &str, held: bool, now: u64) -> Case {
    use crate::sim::Call;
    use serde_json::json;
    let mut c = worker_case();
    match cleanup {
        "train" => {
            c.config.settings.train = true;
            let reports = c.pass(now, false);
            assert_eq!(reports[0].error, None, "{reports:?}");
            assert_eq!(step(&reports, 0).1, Step::JoinTrain);
        }
        "queued" => c.hub.edit_pull(REPO, 3, |p| p["auto_merge"] = json!({})),
        "pending" => {
            // A submitted merge whose visibility has not caught up.
            c.hub.race(Call::Land, move |hub| {
                if held {
                    hub.label(REPO, 3, &["hold"]);
                }
                hub.stale(Call::Pull);
                hub.stale(Call::Pulls);
            });
            c.pass(now, false);
            assert!(record(&c, 3).landing.as_ref().unwrap().accepted);
        }
        "stop" => {}
        _ => unreachable!(),
    }
    c
}
fn cancellation_with_cleanup(cleanup: &str) {
    for held in [false, true] {
        let now = crate::flow::jobs::now();
        let c = cleanup_case(cleanup, held, now);
        let running = workers(&c, now + 1);
        assert_eq!(running.len(), 4);
        if held {
            c.hub.label(REPO, 3, &["hold"]);
        } else {
            pause(&c, "cleanup must also stop workers");
        }
        let reports = c.pass(now + 2, false);
        let expected = match cleanup {
            "train" => Step::LeaveTrain,
            "queued" => Step::DisableAutoMerge,
            _ => Step::Wait { until: None },
        };
        assert_eq!(
            step(&reports, 0).1,
            expected,
            "{held} {cleanup}: {reports:?}"
        );
        for work in &running {
            assert_eq!(
                marker(work),
                if held {
                    b"held\n".as_slice()
                } else {
                    b"paused\n"
                },
                "{held} {cleanup} {:?}",
                work.purpose
            );
        }
        no_failure(&c, 3);
        c.run_worker();
        c.pass(now + 3, false);
        no_failure(&c, 3);
        for work in &running {
            assert!(record(&c, 3).memory.retired_jobs.contains(&work.job.id));
        }
    }
}
#[test]
fn pause_and_hold_cancel_workers_while_removing_queued_merge() {
    cancellation_with_cleanup("queued");
}
#[test]
fn pause_and_hold_cancel_workers_while_leaving_train() {
    cancellation_with_cleanup("train");
}
#[test]
fn pause_and_hold_cancel_workers_while_waiting_for_pending_landing() {
    cancellation_with_cleanup("pending");
}

#[test]
fn repository_pause_cancels_workers_after_the_pr_leaves_open() {
    use serde_json::json;
    for merged in [false, true] {
        let c = worker_case();
        let now = crate::flow::jobs::now();
        let running = workers(&c, now);
        // One finished answer must survive byte for byte, with no marker.
        let mut completed = running[0].clone();
        completed.job.id = "3-0".into();
        let data = Data::open(c.dir.path(), &repo()).unwrap();
        completed.path = data.job_path(3, 0);
        let mut saved = data.load(3).unwrap();
        saved.jobs.push(completed.clone());
        data.save(3, &saved).unwrap();
        drop(data);
        let answer = completed.path.with_extension("answer");
        let bytes = serde_json::to_vec(&crate::flow::jobs::Completion {
            at: now,
            result: Some(Ok(conary_core::plugin::Response {
                contract: conary_core::plugin::CONTRACT.into(),
                by: conary_core::plugin::Identity {
                    engine: "fixture".into(),
                    session: None,
                },
                execution: None,
                answer: conary_core::plugin::Answer::Fix {
                    result: conary_core::plugin::FixResult::Unchanged,
                    answers: Some(vec![]),
                },
            })),
        })
        .unwrap();
        std::fs::write(&answer, &bytes).unwrap();
        c.hub.edit_pull(REPO, 3, |p| {
            p["state"] = json!("closed");
            if merged {
                p["merged_at"] = json!("2099-01-01T00:00:00Z");
            }
        });
        pause(&c, "closed PR workers too");
        c.pass(now + 1, false);
        for work in &running {
            assert_eq!(
                marker(work),
                b"paused\n",
                "merged={merged} {:?}",
                work.purpose
            );
        }
        assert!(!completed.path.with_extension("cancel").exists());
        assert_eq!(std::fs::read(&answer).unwrap(), bytes);
        c.run_worker();
        c.pass(now + 2, false);
        no_failure(&c, 3);
        for work in &running {
            assert_eq!(marker(work), b"paused\n");
        }
    }
}

fn completion_during_cleanup(cleanup: &str) {
    use crate::api::Source;
    use crate::sim::{Call, On};
    use crate::train::Mark;
    for held in [false, true] {
        for call in [Call::FlowStatus, Call::FlowStage] {
            for kind in 0..4 {
                let now = crate::flow::jobs::now();
                let c = cleanup_case(cleanup, held, now);
                let running = workers(&c, now + 1);
                let exiting = running[kind].clone();
                if held {
                    c.hub.label(REPO, 3, &["hold"]);
                } else {
                    pause(&c, "workers can exit during cleanup");
                }
                let finish = move |_: &crate::sim::Hub| {
                    assert!(exiting.path.with_extension("cancel").exists());
                    crate::flow::jobs::worker(&exiting.path).unwrap();
                    assert!(exiting.path.with_extension("answer").exists());
                };
                let on = if call == Call::FlowStatus {
                    On::from(call).mark(Mark::Waiting)
                } else {
                    On::from(call)
                };
                if cleanup == "train" && call == Call::FlowStatus {
                    let again = on.clone();
                    c.hub.race(on, move |hub| hub.race(again, finish));
                } else {
                    c.hub.race(on, finish);
                }
                let reports = c.pass(now + 2, false);
                assert_eq!(
                    reports[0].error, None,
                    "{cleanup} held={held} {call:?} {:?}: {reports:?}",
                    running[kind].purpose
                );
                let expected = match cleanup {
                    "stop" => Step::StopJob(running[0].job.id.clone()),
                    "queued" => Step::DisableAutoMerge,
                    "train" => Step::LeaveTrain,
                    _ => Step::Wait { until: None },
                };
                assert_eq!(step(&reports, 0).1, expected);
                if cleanup == "queued" {
                    assert!(c.hub.pull(REPO, 3).unwrap().auto_merge.is_none());
                }
                if cleanup == "train" {
                    assert!(
                        c.store
                            .borrow()
                            .ledger()
                            .train(&repo())
                            .unwrap()
                            .joined()
                            .is_empty()
                    );
                }
                no_failure(&c, 3);
                c.run_worker();
                c.pass(now + 3, false);
                no_failure(&c, 3);
                if cleanup == "pending" {
                    // Let the accepted merge become visible before resuming.
                    c.hub.fresh(Call::Pull);
                    c.hub.fresh(Call::Pulls);
                }
                if held {
                    c.hub.label(REPO, 3, &[]);
                } else {
                    crate::flow::pause::resume(c.dir.path(), &repo()).unwrap();
                }
                c.pass(now + 4, false);
                no_failure(&c, 3);
            }
        }
    }
}
#[test]
fn cancelled_workers_can_exit_during_stop_validation() {
    completion_during_cleanup("stop");
}
#[test]
fn cancelled_workers_can_exit_during_queued_merge_cleanup_validation() {
    completion_during_cleanup("queued");
}
#[test]
fn cancelled_workers_can_exit_during_train_departure_validation() {
    completion_during_cleanup("train");
}
#[test]
fn cancelled_workers_can_exit_during_non_granting_wait_validation() {
    completion_during_cleanup("pending");
}

#[test]
fn permission_still_rechecks_exiting_cancelled_workers() {
    use crate::api::Source;
    use crate::sim::{Call, On};
    use crate::train::Mark;
    for mode in ["direct", "delegated", "join", "seated"] {
        for call in [Call::FlowStatus, Call::FlowStage] {
            let now = crate::flow::jobs::now();
            let mut c = worker_case();
            match mode {
                "delegated" => c.as_bot(),
                "join" | "seated" => c.config.settings.train = true,
                _ => {}
            }
            if mode == "seated" {
                let reports = c.pass(now, false);
                assert_eq!(reports[0].error, None, "{reports:?}");
                assert_eq!(step(&reports, 0).1, Step::JoinTrain);
            }
            // An independently attested clean review is already visible,
            // while an owned proof, which holds no landing here, is still
            // exiting after cancellation.
            let work = start_workers(&c, now + 1, 3..4).pop().unwrap();
            crate::flow::jobs::cancel(&work, crate::flow::pause::Stop::Obsolete).unwrap();
            let finish = move |_: &crate::sim::Hub| {
                crate::flow::jobs::worker(&work.path).unwrap();
            };
            let on = if call == Call::FlowStatus {
                On::from(call).mark(Mark::Waiting)
            } else {
                On::from(call)
            };
            if mode == "seated" && call == Call::FlowStatus {
                let again = on.clone();
                c.hub.race(on, move |hub| hub.race(again, finish));
            } else {
                c.hub.race(on, finish);
            }
            let reports = c.pass(now + 2, false);
            let expected = match mode {
                "direct" => Step::Merge,
                "delegated" => Step::Settle,
                "join" => Step::JoinTrain,
                _ => Step::Wait { until: None },
            };
            assert_eq!(step(&reports, 0).1, expected, "{mode}: {reports:?}");
            assert!(
                reports[0]
                    .error
                    .as_deref()
                    .is_some_and(|e| e.contains("scope changed")),
                "{mode}: {reports:?}"
            );
            assert_eq!(c.hub.count(Call::Land), 0);
            assert_eq!(
                c.hub
                    .statuses(REPO, &super::head(3))
                    .unwrap()
                    .iter()
                    .find(|s| s.context == SETTLED)
                    .unwrap()
                    .state,
                "pending"
            );
        }
    }
}
