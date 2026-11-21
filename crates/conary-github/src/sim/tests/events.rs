//! Event source regression tests are wired from the pre-existing sim test root,
//! so the negative tree also discovers them.
use crate::events::*;
use crate::flow::{Level, Next, Report, Stage, Step};
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
            queued: None,
            recovery: None,
            proof: None,
            wait_on: None,
            on_usage: false,
            owner_notice: None,
            switches: vec![],
            priority: Level::Medium,
            degraded: Vec::new(),
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
    let mut receiver = Receiver::open(
        b"secret".to_vec(),
        Box::new(|| Ok(vec!["o/r".into()])),
        dir.path(),
    )
    .unwrap();
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
    let mut receiver = Receiver::open(
        b"s".to_vec(),
        Box::new(|| Ok(vec!["o/r".into()])),
        dir.path(),
    )
    .unwrap();
    let body = payload();
    let sig = signature(b"s", &body);
    assert_eq!(
        receiver.receive(Some(&sig), "id", "pull_request", &body, 1, &queue),
        202
    );
    let work = queue.take(1).unwrap().unwrap();
    queue.finish(&work, &[], 1).unwrap();
    drop(receiver);
    let mut receiver = Receiver::open(
        b"s".to_vec(),
        Box::new(|| Ok(vec!["o/r".into()])),
        dir.path(),
    )
    .unwrap();
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
    let mut receiver = Receiver::open(
        b"s".to_vec(),
        Box::new(|| Ok(vec![REPO.into()])),
        dir.path(),
    )
    .unwrap();
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
                    // The flow first sees the head now: it waits for the
                    // connector's review of the push.
                    let next = reports[0].next.as_ref().unwrap();
                    assert_eq!(
                        next.step,
                        Step::Wait {
                            until: Some(now + 1800)
                        }
                    );
                    assert_eq!(next.wait_until, Some(now + 1800));
                    assert_eq!(c.record().memory.seen, now, "the pass kept what it saw");
                    // It wrote the PR's stage and status on the hub.
                    assert_eq!(
                        (
                            c.hub.count(crate::sim::Call::FlowStage),
                            c.hub.count(crate::sim::Call::FlowStatus)
                        ),
                        (1, 1)
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

fn runtime(dir: &std::path::Path, worker: &str) -> Runtime {
    serde_json::from_value(json!({
        "listen": "127.0.0.1:0",
        "secret_path": dir.join("secret"),
        "repos": ["o/r", "o/s"],
        "data": dir,
        "flow_config": dir.join("flow.json"),
        "policy": dir.join("policy.json"),
        "token_command": "true",
        "anchor_repo": null,
        "worker": worker,
    }))
    .unwrap()
}
/// A thread asleep on the mailbox, as conaryd's worker is, that sends the
/// first work it's woken for.
fn sleeper(
    queue: &std::sync::Arc<Queue>,
) -> (std::sync::mpsc::Receiver<Work>, std::thread::JoinHandle<()>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let waiting = std::sync::Arc::clone(queue);
    let thread = std::thread::spawn(move || {
        loop {
            if let Some(work) = waiting.take(crate::flow::jobs::now()).unwrap() {
                tx.send(work).unwrap();
                break;
            }
            waiting.wait();
        }
    });
    (rx, thread)
}

/// Only a pass reads a worker's answer, and most publish nothing GitHub
/// sends an event for, so its exit wakes its repository's pass: answered,
/// failed, or stopped by its cancel marker.
#[test]
fn a_workers_exit_wakes_its_repositorys_pass_however_it_ended() {
    use crate::flow::jobs::Launch;
    use std::time::Duration;
    for (name, body) in [
        ("answered", "exit 0"),
        ("failed", "exit 1"),
        // Runs until it finds its cancel marker, as the plugin runner does.
        (
            "cancelled",
            r#"while [ ! -e "${2%.job}.cancel" ]; do sleep 0.05; done"#,
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let scripts = tempfile::tempdir().unwrap();
        let worker = crate::tokens::tests::script(scripts.path(), body);
        let runtime = runtime(dir.path(), &worker);
        let queue = std::sync::Arc::new(Queue::open(dir.path()).unwrap());
        let (woken, thread) = sleeper(&queue);
        let job = dir.path().join("3-1.job");
        runtime.launcher("o/r", &queue).launch(&job).unwrap();
        if name == "cancelled" {
            assert!(
                woken.recv_timeout(Duration::from_millis(500)).is_err(),
                "a running worker woke the pass"
            );
            std::fs::write(job.with_extension("cancel"), b"obsolete\n").unwrap();
        }
        let work = woken
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|_| panic!("a {name} worker's exit woke nothing"));
        assert_eq!(work.repo, "o/r");
        thread.join().unwrap();
        queue.finish(&work, &[], crate::flow::jobs::now()).unwrap();
        // Its kick was the only one.
        assert!(queue.take(crate::flow::jobs::now()).unwrap().is_none());
    }
}

/// A worker that answered while conaryd was down is read as it starts again.
#[test]
fn the_daemon_reads_every_repository_once_as_it_starts() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(dir.path(), "/nonexistent");
    let queue = std::sync::Arc::new(Queue::open(dir.path()).unwrap());
    assert!(queue.take(1).unwrap().is_none());
    runtime.restart(&queue).unwrap();
    let now = crate::flow::jobs::now();
    let mut repos = Vec::new();
    while let Some(work) = queue.take(now).unwrap() {
        repos.push(work.repo.clone());
        queue.finish(&work, &[], now).unwrap();
    }
    assert_eq!(repos, ["o/r", "o/s"]);
}

/// A worker an earlier daemon launched, still running as the next one
/// starts, wakes that daemon's pass as it exits: answered, failed, or
/// cancelled by the new daemon. Only the earlier daemon reaped it, and it's
/// gone.
#[test]
fn a_worker_that_outlives_its_daemon_wakes_the_next_ones_pass() {
    use crate::flow::jobs::{Launch, Process};
    use std::time::Duration;
    for (name, body) in [
        (
            "answered",
            r#"while [ ! -e "${2%.job}.go" ]; do sleep 0.05; done; echo '{"at":1,"result":null}' > "${2%.job}.answer""#,
        ),
        (
            "failed",
            r#"while [ ! -e "${2%.job}.go" ]; do sleep 0.05; done; exit 1"#,
        ),
        (
            "cancelled",
            r#"while [ ! -e "${2%.job}.cancel" ]; do sleep 0.05; done"#,
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let scripts = tempfile::tempdir().unwrap();
        let worker = crate::tokens::tests::script(scripts.path(), body);
        let runtime = runtime(dir.path(), &worker);
        let flow = dir.path().join("flow").join("o").join("r");
        std::fs::create_dir_all(&flow).unwrap();
        let job = flow.join("3-1.job");
        std::fs::write(&job, b"{}").unwrap();
        // A finished worker's invocation, already answered, isn't watched.
        std::fs::write(flow.join("2-1.job"), b"{}").unwrap();
        std::fs::write(flow.join("2-1.answer"), b"{}").unwrap();
        // The earlier daemon, whose reaper died with it.
        Process {
            worker: worker.clone().into(),
            ended: None,
        }
        .launch(&job)
        .unwrap();
        let queue = std::sync::Arc::new(Queue::open(dir.path()).unwrap());
        runtime.restart(&queue).unwrap();
        let now = crate::flow::jobs::now();
        while let Some(work) = queue.take(now).unwrap() {
            queue.finish(&work, &[], now).unwrap();
        }
        let (woken, thread) = sleeper(&queue);
        assert!(
            woken.recv_timeout(Duration::from_millis(500)).is_err(),
            "a running {name} worker woke the pass"
        );
        let release = if name == "cancelled" { "cancel" } else { "go" };
        std::fs::write(job.with_extension(release), b"\n").unwrap();
        let work = woken
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|_| panic!("a {name} worker that outlived its daemon woke nothing"));
        assert_eq!(work.repo, "o/r");
        thread.join().unwrap();
        queue.finish(&work, &[], crate::flow::jobs::now()).unwrap();
        assert!(queue.take(crate::flow::jobs::now()).unwrap().is_none());
    }
}

/// One conaryd lands two repositories, each under a flow configuration of
/// its own: a pass reads its repository's file and no other, at the pass,
/// so one repository's evidence, train or broken file never reaches the
/// other's. The file that gives every repository one stays valid, and the
/// events file is refused where its repositories and their files disagree.
#[test]
fn each_repository_passes_under_its_own_flow_configuration() {
    use std::sync::Arc;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let ledger = root.join("ledger");
    conary_core::log::load_or_create_key(&conary_core::log::key_path(&ledger)).unwrap();
    std::fs::write(root.join("policy.json"), b"{}").unwrap();
    let path = root.join("events.json");
    let events = |repos: serde_json::Value, flow_config: serde_json::Value| {
        let config = json!({
            "listen": "127.0.0.1:0",
            "secret_path": root.join("secret"),
            "repos": repos,
            "data": root.join("data"),
            "flow_config": flow_config,
            "policy": root.join("policy.json"),
            "token_command": "true",
            "anchor_repo": null,
            "worker": "/nonexistent",
            "api": "http://127.0.0.1:1",
        });
        std::fs::write(&path, config.to_string()).unwrap();
    };
    let flow = |name: &str, config: serde_json::Value| {
        std::fs::write(root.join(name), config.to_string()).unwrap();
    };
    let evidence = json!({"evidence": {
        "signer": "o/ci/.github/workflows/gate.yml",
        "require": ["gate"],
        "negative": "negative",
    }});
    let train = json!({"settings": {"train": true}});
    let each = json!({"o/r": root.join("r.json"), "O/S": root.join("s.json")});
    flow("r.json", evidence.clone());
    flow("s.json", json!({}));
    events(json!(["o/r", "o/s"]), each.clone());
    let live = Live::open(&path).unwrap();
    let queue = Arc::new(Queue::open(&root.join("data")).unwrap());
    // Every pass here fails before GitHub, and its error says what its flow
    // needs: one with evidence or the train on the anchor_repo none is
    // given, one with neither on reading the ledger, never anchored.
    let pass = |repo: &str| {
        live.work(&ledger, &Work { repo: repo.into() }, &queue)
            .unwrap_err()
            .to_string()
    };
    let needs_anchor = |repo: &str| {
        let error = pass(repo);
        match (
            error.contains("require anchor_repo"),
            error.contains("no anchor for the op log"),
        ) {
            (true, false) => true,
            (false, true) => false,
            _ => panic!("{repo}'s pass failed on neither: {error}"),
        }
    };

    // o/r's evidence is its own: o/s, with none, needs no ledger.
    assert!(needs_anchor("o/r"), "o/r's evidence needs the ledger");
    assert!(!needs_anchor("o/s"), "o/r's evidence reached o/s");

    // Each file is read at its repository's pass: the train turned on for
    // o/s alone, and o/r's evidence taken away, swap them.
    flow("r.json", json!({}));
    flow("s.json", train.clone());
    assert!(needs_anchor("o/s"), "o/s's train needs the ledger");
    assert!(!needs_anchor("o/r"), "o/s's train reached o/r");

    // A file that doesn't load fails its own repository's pass alone.
    flow("s.json", json!({"nope": 1}));
    let broken = pass("o/s");
    assert!(broken.contains("doesn't declare"), "{broken}");
    flow("r.json", evidence);
    let other = pass("o/r");
    assert!(other.contains("require anchor_repo"), "{other}");

    // One file for every repository stays valid.
    flow("flow.json", train);
    events(json!(["o/r", "o/s"]), json!(root.join("flow.json")));
    assert!(needs_anchor("o/r") && needs_anchor("o/s"));

    // A listed repository with no file, a file for one not listed, one
    // repository named twice and a key that isn't one are refused, by the
    // daemon as it starts and at the next pass.
    for (repos, flow_config, refused) in [
        (
            json!(["o/r", "o/s", "o/t"]),
            each.clone(),
            "flow_config gives o/t no file",
        ),
        (
            json!(["o/r"]),
            each.clone(),
            "flow_config names a repository repos doesn't list",
        ),
        (
            json!(["o/r", "o/s"]),
            json!({"o/r": "/r", "o/s": "/s", "O/s": "/s"}),
            "names o/s twice",
        ),
        (
            json!(["o/r", "o/s"]),
            json!({"o/r": "/r", "o/s": "/s", "o": "/t"}),
            "a key that isn't OWNER/REPO",
        ),
    ] {
        events(repos, flow_config);
        let open = Live::open(&path).err().map(|e| e.to_string());
        assert!(
            open.as_deref().is_some_and(|e| e.contains(refused)),
            "{open:?}"
        );
        let read = live.read().err().map(|e| e.to_string());
        assert!(
            read.as_deref().is_some_and(|e| e.contains(refused)),
            "{read:?}"
        );
    }
}
