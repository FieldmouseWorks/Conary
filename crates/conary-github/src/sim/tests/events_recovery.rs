//! A whole-repository pass that fails books its repo's recovery: the worker
//! goes on, other repos advance, and the failed repo's kicks and bookings
//! wait for its retry, across restarts too.
use crate::events::{Kick, Queue, Ran, Recovery};
use crate::flow::{Next, Report, Stage, Step};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

fn kick(queue: &Queue, repo: &str) {
    queue
        .kick(Kick {
            repo: repo.into(),
            pr: Some(3),
        })
        .unwrap();
}
fn booked(at: u64) -> Report {
    Report {
        repo: "o/r".parse().unwrap(),
        pr: 3,
        next: Some(Next {
            stage: Stage::Ci,
            step: Step::Wait { until: Some(at) },
            description: "waiting".into(),
            wait_until: Some(at),
            overdue_at: None,
            proof: None,
            wait_on: None,
            on_usage: false,
            owner_notice: None,
        }),
        error: None,
        owner_approval: None,
        revision: None,
    }
}
fn unavailable(_: &crate::events::Work) -> crate::flow::persist::Result<Vec<Report>> {
    Err("GitHub unavailable".into())
}

#[test]
fn a_pass_failing_before_reports_books_recovery_instead_of_stopping() {
    let dir = tempfile::tempdir().unwrap();
    let queue = Queue::open(dir.path()).unwrap();
    kick(&queue, "o/r");
    assert_eq!(
        queue.run_one(10, unavailable).unwrap(),
        Ran::Failed {
            repo: "o/r".into(),
            recovery: Recovery {
                at: 70,
                attempts: 1
            }
        }
    );
    assert_eq!(queue.recovery("o/r").map(|r| r.at), Some(70));
    assert_eq!(queue.run_one(69, unavailable).unwrap(), Ran::Idle);
    assert_eq!(queue.take(70).unwrap().unwrap().repo, "o/r");
}

#[test]
fn retries_back_off_to_a_bound_and_a_finished_pass_resets_them() {
    let dir = tempfile::tempdir().unwrap();
    let queue = Queue::open(dir.path()).unwrap();
    kick(&queue, "o/r");
    let mut now = 1000;
    for (attempts, delay) in [60, 120, 240, 480, 960, 1920, 3840, 3840, 3840]
        .into_iter()
        .enumerate()
    {
        if attempts > 0 {
            assert!(
                queue.take(now - 1).unwrap().is_none(),
                "not before its retry"
            );
        }
        let work = queue.take(now).unwrap().expect("its recovery is due");
        let recovery = queue.fail(&work, now).unwrap();
        assert_eq!(recovery.at, now + delay, "attempt {}", attempts + 1);
        assert_eq!(recovery.attempts as usize, attempts + 1);
        now = recovery.at;
    }
    let work = queue.take(now).unwrap().unwrap();
    queue.finish(&work, &[], now).unwrap();
    assert_eq!(queue.recovery("o/r"), None);
    assert!(queue.take(now + 100_000).unwrap().is_none(), "no scan");
    kick(&queue, "o/r");
    let work = queue.take(now).unwrap().unwrap();
    assert_eq!(queue.fail(&work, now).unwrap().at, now + 60);
}

#[test]
fn kicks_during_a_failure_wait_for_its_recovery_and_make_one_pass() {
    let dir = tempfile::tempdir().unwrap();
    let queue = Queue::open(dir.path()).unwrap();
    kick(&queue, "o/r");
    let ran = queue
        .run_one(10, |_| {
            kick(&queue, "o/r"); // during the pass that fails
            Err("GitHub unavailable".into())
        })
        .unwrap();
    assert!(matches!(ran, Ran::Failed { .. }));
    for _ in 0..5 {
        kick(&queue, "o/r");
    }
    queue.status("o/r", 7).unwrap();
    assert!(queue.take(69).unwrap().is_none(), "held until its recovery");
    let work = queue.take(70).unwrap().expect("its kicks and recovery");
    queue.finish(&work, &[], 70).unwrap();
    assert!(
        queue.take(71).unwrap().is_none(),
        "one pass for all of them"
    );
}

#[test]
fn a_failed_repos_bookings_are_held_not_spent_until_its_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let queue = Queue::open(dir.path()).unwrap();
    kick(&queue, "o/r");
    let work = queue.take(1).unwrap().unwrap();
    queue.finish(&work, &[booked(100)], 1).unwrap();
    kick(&queue, "o/r");
    let work = queue.take(50).unwrap().unwrap();
    assert_eq!(queue.fail(&work, 50).unwrap().at, 110);
    assert!(queue.take(100).unwrap().is_none(), "its booking is held");
    let work = queue.take(110).unwrap().expect("recovery");
    assert_eq!(queue.fail(&work, 110).unwrap().at, 230);
    assert!(queue.take(229).unwrap().is_none());
    let work = queue.take(230).unwrap().expect("recovery");
    queue.finish(&work, &[booked(300)], 230).unwrap();
    assert!(
        queue.take(299).unwrap().is_none(),
        "the old booking was spent"
    );
    assert!(queue.take(300).unwrap().is_some());
}

#[test]
fn recovery_kicks_and_bookings_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let queue = Queue::open(dir.path()).unwrap();
    kick(&queue, "o/r");
    let work = queue.take(1).unwrap().unwrap();
    queue.finish(&work, &[booked(500)], 1).unwrap();
    kick(&queue, "o/r");
    let work = queue.take(10).unwrap().unwrap();
    kick(&queue, "o/r");
    queue.fail(&work, 10).unwrap();
    drop(queue);
    let queue = Queue::open(dir.path()).unwrap();
    assert_eq!(
        queue.recovery("o/r"),
        Some(Recovery {
            at: 70,
            attempts: 1
        })
    );
    assert!(
        queue.take(69).unwrap().is_none(),
        "still held after restart"
    );
    let work = queue.take(70).unwrap().expect("recovery after restart");
    // Stopped mid-retry: the active pass is recovered, attempts kept.
    drop(queue);
    let queue = Queue::open(dir.path()).unwrap();
    let work2 = queue.take(80).unwrap().expect("in-flight retry recovered");
    assert_eq!(work, work2);
    assert_eq!(queue.fail(&work2, 80).unwrap().attempts, 2);
    let work = queue.take(200).unwrap().unwrap();
    queue.finish(&work, &[booked(500)], 200).unwrap();
    assert!(queue.take(499).unwrap().is_none());
    assert!(queue.take(500).unwrap().is_some(), "the booking survived");
}

#[test]
fn other_repos_advance_while_one_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let queue = Queue::open(dir.path()).unwrap();
    kick(&queue, "a/x");
    kick(&queue, "o/r");
    let failed = queue.take(10).unwrap().unwrap();
    assert_eq!(failed.repo, "a/x");
    queue.fail(&failed, 10).unwrap();
    kick(&queue, "a/x");
    let work = queue.take(11).unwrap().expect("the other repo goes on");
    assert_eq!(work.repo, "o/r");
    queue.finish(&work, &[], 11).unwrap();
    kick(&queue, "o/r");
    assert_eq!(queue.take(12).unwrap().unwrap().repo, "o/r");
}

#[test]
fn the_sleeper_ignores_a_held_repos_kicks_and_wakes_for_its_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let queue = Queue::open(dir.path()).unwrap();
    let now = crate::flow::jobs::now();
    kick(&queue, "o/r");
    let work = queue.take(now - 100).unwrap().unwrap();
    queue.finish(&work, &[booked(now - 50)], now - 100).unwrap();
    kick(&queue, "o/r");
    let work = queue.take(now - 58).unwrap().unwrap();
    kick(&queue, "o/r");
    // Retries two seconds from now, with a kick queued and a booking due.
    assert_eq!(queue.fail(&work, now - 58).unwrap().at, now + 2);
    let started = Instant::now();
    queue.wait();
    assert!(
        started.elapsed() >= Duration::from_millis(500),
        "a held repo's kick or booking must not wake the worker"
    );
    let mut work = None;
    while work.is_none() && started.elapsed() < Duration::from_secs(5) {
        work = queue.take(crate::flow::jobs::now()).unwrap();
        if work.is_none() {
            queue.wait();
        }
    }
    assert!(work.is_some(), "the recovery wakes it");
}

#[test]
fn the_worker_books_a_failed_pass_and_goes_on_serving() {
    let dir = tempfile::tempdir().unwrap();
    let queue = Arc::new(Queue::open(dir.path()).unwrap());
    kick(&queue, "a/x");
    kick(&queue, "o/r");
    let (tx, rx) = mpsc::channel();
    let worker = {
        let queue = Arc::clone(&queue);
        let failed = tx.clone();
        std::thread::spawn(move || {
            queue.work(
                |work| {
                    if work.repo == "a/x" {
                        return Err("GitHub unavailable".into());
                    }
                    tx.send(format!("passed {}", work.repo)).unwrap();
                    Ok(vec![])
                },
                |repo, _| failed.send(format!("failed {repo}")).unwrap(),
            )
        })
    };
    let next = || rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(next(), "failed a/x");
    assert_eq!(next(), "passed o/r");
    assert!(queue.recovery("a/x").is_some(), "a/x's recovery is booked");
    kick(&queue, "a/x");
    kick(&queue, "o/r");
    assert_eq!(next(), "passed o/r", "the worker is still serving");
    assert!(
        !worker.is_finished(),
        "a failed pass must not stop the worker"
    );
}
