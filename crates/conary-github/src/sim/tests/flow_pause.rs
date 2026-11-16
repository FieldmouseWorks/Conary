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
