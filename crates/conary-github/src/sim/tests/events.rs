//! Event source regression tests are wired from the pre-existing sim test root,
//! so the negative tree also discovers them.
use crate::events::*;
use crate::flow::{Next, Report, Stage, Step};
use ring::hmac;
use serde_json::json;

fn signature(secret: &[u8], body: &[u8]) -> String {
    let tag = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, secret), body);
    format!(
        "sha256={}",
        tag.as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}
fn payload() -> Vec<u8> {
    serde_json::to_vec(&json!({"repository":{"full_name":"o/r"},"pull_request":{"number":3}}))
        .unwrap()
}
fn kick() -> Kick {
    Kick {
        repo: "o/r".into(),
        pr: Some(3),
    }
}
fn report(at: u64) -> Report {
    Report {
        repo: "o/r".parse().unwrap(),
        pr: 3,
        next: Some(Next {
            stage: Stage::Ci,
            step: Step::Wait { until: Some(at) },
            description: "waiting".into(),
            wait_until: Some(at),
            overdue_at: Some(at + 50),
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
#[test]
fn signatures_reject_before_queueing() {
    let dir = tempfile::tempdir().unwrap();
    let queue = Queue::open(dir.path()).unwrap();
    let mut receiver = Receiver::open(b"secret".to_vec(), ["o/r".into()], dir.path()).unwrap();
    let body = payload();
    let good = signature(b"secret", &body);
    let wrong = signature(b"wrong", &body);
    for sig in [None, Some(wrong.as_str()), Some("sha1=123")] {
        assert_eq!(
            receiver.receive(sig, "id", "pull_request", &body, 1, &queue),
            401
        );
        assert!(queue.take(1).unwrap().is_none());
    }
    let mut altered = body.clone();
    altered[0] ^= 1;
    assert_eq!(
        receiver.receive(Some(&good), "id", "pull_request", &altered, 1, &queue),
        401
    );
    assert!(queue.take(1).unwrap().is_none());
    assert_eq!(
        receiver.receive(Some(&good), "id", "pull_request", &body, 1, &queue),
        202
    );
    assert!(queue.take(1).unwrap().is_some());
}
#[test]
fn replay_survives_restart_and_size_and_allowlist_are_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let queue = Queue::open(dir.path()).unwrap();
    let mut receiver = Receiver::open(b"s".to_vec(), ["o/r".into()], dir.path()).unwrap();
    let body = payload();
    let sig = signature(b"s", &body);
    assert_eq!(
        receiver.receive(Some(&sig), "id", "pull_request", &body, 1, &queue),
        202
    );
    let work = queue.take(1).unwrap().unwrap();
    queue.finish(&work, &[], 1).unwrap();
    drop(receiver);
    let mut receiver = Receiver::open(b"s".to_vec(), ["o/r".into()], dir.path()).unwrap();
    assert_eq!(
        receiver.receive(Some(&sig), "id", "pull_request", &body, 2, &queue),
        202
    );
    assert!(queue.take(2).unwrap().is_none());
    let huge = vec![b'x'; MAX_BODY + 1];
    assert_eq!(
        receiver.receive(None, "huge", "pull_request", &huge, 2, &queue),
        413
    );
    let other = br#"{"repository":{"full_name":"elsewhere/repo"},"pull_request":{"number":3}}"#;
    assert_eq!(
        receiver.receive(
            Some(&signature(b"s", other)),
            "other",
            "pull_request",
            other,
            2,
            &queue
        ),
        202
    );
    assert!(queue.take(2).unwrap().is_none());
}
#[test]
fn event_table_maps_trimmed_synthetic_github_shapes() {
    let repos = ["o/r".into()].into_iter().collect();
    let fixtures: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/webhooks.json")).unwrap();
    for row in fixtures.as_array().unwrap() {
        let expected = row["prs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pr| Kick {
                repo: "o/r".into(),
                pr: pr.as_u64(),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            kicks(row["event"].as_str().unwrap(), &row["payload"], &repos),
            expected,
            "{}",
            row["event"]
        );
    }
}
#[test]
fn burst_coalesces_to_one_pass() {
    let dir = tempfile::tempdir().unwrap();
    let queue = Queue::open(dir.path()).unwrap();
    for _ in 0..20 {
        queue.kick(kick()).unwrap();
    }
    let work = queue.take(1).unwrap().unwrap();
    queue.finish(&work, &[], 1).unwrap();
    assert!(queue.take(1).unwrap().is_none());
}
#[test]
fn kick_during_pass_is_exactly_one_more() {
    let dir = tempfile::tempdir().unwrap();
    let queue = Queue::open(dir.path()).unwrap();
    queue.kick(kick()).unwrap();
    let first = queue.take(1).unwrap().unwrap();
    for _ in 0..20 {
        queue.kick(kick()).unwrap();
    }
    assert!(queue.take(1).unwrap().is_none());
    queue.finish(&first, &[], 1).unwrap();
    let second = queue
        .take(1)
        .unwrap()
        .expect("kick during active pass must survive finish");
    queue.finish(&second, &[], 1).unwrap();
    assert!(queue.take(1).unwrap().is_none());
}
#[test]
fn booked_wait_survives_restart_replaces_and_fires_once() {
    let dir = tempfile::tempdir().unwrap();
    let queue = Queue::open(dir.path()).unwrap();
    queue.kick(kick()).unwrap();
    let work = queue.take(1).unwrap().unwrap();
    queue.finish(&work, &[report(100)], 1).unwrap();
    queue.kick(kick()).unwrap();
    let work = queue.take(2).unwrap().unwrap();
    queue.finish(&work, &[report(200)], 2).unwrap();
    drop(queue);
    let queue = Queue::open(dir.path()).unwrap();
    assert!(queue.take(100).unwrap().is_none());
    assert!(queue.take(199).unwrap().is_none());
    let work = queue.take(200).unwrap().expect("booked deadline");
    queue.finish(&work, &[report(200)], 200).unwrap();
    assert!(queue.take(200).unwrap().is_none());
    let overdue = queue.take(250).unwrap().expect("overdue booking");
    queue.finish(&overdue, &[report(200)], 250).unwrap();
    assert!(queue.take(251).unwrap().is_none());
}
#[test]
fn in_flight_work_is_recovered_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let queue = Queue::open(dir.path()).unwrap();
    queue.kick(kick()).unwrap();
    assert!(queue.take(1).unwrap().is_some());
    drop(queue);
    let queue = Queue::open(dir.path()).unwrap();
    assert!(queue.take(2).unwrap().is_some());
}
#[test]
fn signed_pull_event_runs_flow_against_hub() {
    use super::flow_pass_case::{Case, REPO};
    let c = Case::new();
    let dir = tempfile::tempdir().unwrap();
    let queue = Queue::open(dir.path()).unwrap();
    let mut receiver = Receiver::open(b"s".to_vec(), [REPO.into()], dir.path()).unwrap();
    let body =
        serde_json::to_vec(&json!({"repository":{"full_name":REPO}, "pull_request":{"number":3}}))
            .unwrap();
    assert_eq!(
        receiver.receive(
            Some(&signature(b"s", &body)),
            "hub",
            "pull_request",
            &body,
            1,
            &queue
        ),
        202
    );
    let now = crate::flow::jobs::now();
    assert_eq!(
        queue
            .run_one(
                || now,
                |work| {
                    assert_eq!(work.repo, REPO);
                    let reports = c.pass(now, false);
                    assert_eq!(
                        reports[0].next.as_ref().unwrap().step,
                        Step::QueueReview("reviewer".into())
                    );
                    assert_eq!(
                        reports[0].next.as_ref().unwrap().wait_until,
                        Some(c.record().jobs[0].deadline)
                    );
                    assert_eq!(
                        c.deferred.0.borrow().len(),
                        1,
                        "the pass executed its plugin launch"
                    );
                    Ok(reports)
                }
            )
            .unwrap(),
        Ran::Finished
    );
    assert!(queue.take(now).unwrap().is_none());
}

#[test]
fn sleeper_wakes_at_a_booked_time_and_on_a_new_kick() {
    use std::sync::{Arc, mpsc};
    use std::time::Duration;
    let dir = tempfile::tempdir().unwrap();
    let queue = Arc::new(Queue::open(dir.path()).unwrap());
    let now = crate::flow::jobs::now();
    queue.kick(kick()).unwrap();
    let work = queue.take(now).unwrap().unwrap();
    queue.finish(&work, &[report(now + 1)], now).unwrap();
    let (tx, rx) = mpsc::channel();
    let waiting = Arc::clone(&queue);
    let worker = std::thread::spawn(move || {
        loop {
            if let Some(work) = waiting.take(crate::flow::jobs::now()).unwrap() {
                tx.send(work).unwrap();
                break;
            }
            waiting.wait();
        }
    });
    let work = rx
        .recv_timeout(Duration::from_secs(3))
        .expect("booked wait must wake without a webhook");
    assert!(crate::flow::jobs::now() > now);
    worker.join().unwrap();
    queue.finish(&work, &[], crate::flow::jobs::now()).unwrap();
    let (tx, rx) = mpsc::channel();
    let waiting = Arc::clone(&queue);
    let worker = std::thread::spawn(move || {
        loop {
            if let Some(work) = waiting.take(crate::flow::jobs::now()).unwrap() {
                tx.send(work).unwrap();
                break;
            }
            waiting.wait();
        }
    });
    queue.kick(kick()).unwrap();
    assert!(rx.recv_timeout(Duration::from_secs(3)).is_ok());
    worker.join().unwrap();
}
