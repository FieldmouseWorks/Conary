//! Regression coverage for receiver review findings.
use crate::events::{Kick, Queue, Runtime};
use crate::flow::{Next, Report, Stage, Step};
fn kick() -> Kick {
    Kick {
        repo: "o/r".into(),
        pr: None,
    }
}
fn report(pr: u64, at: u64, error: bool, next: bool) -> Report {
    Report {
        repo: "o/r".parse().unwrap(),
        pr,
        error: error.then(|| "temporary failure".into()),
        owner_approval: None,
        next: next.then_some(Next {
            stage: Stage::Ci,
            step: Step::Wait { until: Some(at) },
            wait_until: Some(at),
            overdue_at: None,
            description: "waiting".into(),
            proof: None,
            wait_on: None,
            on_usage: false,
            owner_notice: None,
        }),
    }
}
#[test]
fn failed_reports_before_and_after_next_remain_booked_across_restart() {
    for deadline in [false, true] {
        for has_next in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let queue = Queue::open(dir.path()).unwrap();
            queue.kick(kick()).unwrap();
            let work = queue.take(1).unwrap().unwrap();
            let now = if deadline {
                queue
                    .finish(&work, &[report(3, 100, false, true)], 1)
                    .unwrap();
                assert!(queue.take(99).unwrap().is_none());
                assert!(queue.take(100).unwrap().is_some());
                100
            } else {
                1
            };
            queue
                .finish(
                    &work,
                    &[
                        report(3, now + 500, true, has_next),
                        report(4, now + 30, false, true),
                    ],
                    now,
                )
                .unwrap();
            drop(queue);
            let queue = Queue::open(dir.path()).unwrap();
            // The other PR's successful booking is preserved.
            let work = queue.take(now + 30).unwrap().unwrap();
            // Failing again books a new owned retry rather than losing work.
            queue
                .finish(&work, &[report(3, now + 500, true, has_next)], now + 30)
                .unwrap();
            drop(queue);
            let queue = Queue::open(dir.path()).unwrap();
            assert!(queue.take(now + 149).unwrap().is_none());
            let work = queue
                .take(now + 150)
                .unwrap()
                .expect("failed PR retry must survive restart");
            queue.finish(&work, &[], now + 150).unwrap(); // Read established closure.
            assert!(queue.take(now + 1000).unwrap().is_none());
        }
    }
}
#[test]
fn failed_report_without_any_other_booking_gets_a_retry() {
    for has_next in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let queue = Queue::open(dir.path()).unwrap();
        queue.kick(kick()).unwrap();
        let work = queue.take(1).unwrap().unwrap();
        queue
            .finish(&work, &[report(3, 500, true, has_next)], 1)
            .unwrap();
        drop(queue);
        let queue = Queue::open(dir.path()).unwrap();
        assert!(queue.take(60).unwrap().is_none());
        let work = queue.take(61).unwrap().unwrap();
        queue
            .finish(&work, &[report(3, 200, false, true)], 61)
            .unwrap();
        assert!(
            queue.take(181).unwrap().is_none(),
            "success cancels failure backoff"
        );
        assert!(queue.take(200).unwrap().is_some());
    }
}
#[test]
fn quiet_handoff_books_the_first_second_that_allows_progress() {
    use super::flow_case::Case;
    use crate::flow::{Kind, Memory};
    let mut c = Case::new();
    c.threads(1);
    c.memory = Memory::default();
    let before = c.pass(10000);
    assert_eq!(before.wait_until, Some(11801));
    assert!(matches!(c.view(11800).step, Step::Wait { .. }));
    assert_eq!(c.view(11801).step, Step::HandFix(Kind::Findings));
    assert_eq!(c.view(11802).step, Step::HandFix(Kind::Findings));
    let dir = tempfile::tempdir().unwrap();
    let queue = Queue::open(dir.path()).unwrap();
    queue.kick(kick()).unwrap();
    let work = queue.take(10000).unwrap().unwrap();
    let mut r = report(3, 11801, false, true);
    r.next = Some(before);
    queue.finish(&work, &[r], 10000).unwrap();
    assert!(queue.take(11800).unwrap().is_none());
    assert!(queue.take(11801).unwrap().is_some());
}
#[test]
fn retries_and_explicit_waits_wake_at_their_first_permitted_second() {
    use super::flow_case::Case;
    use crate::flow::{Failure, Memory};
    let mut c = Case::new();
    c.threads(1);
    c.memory = Memory::default();
    c.pass(10000);
    c.memory.retry = Some((Failure::Status, 12000));
    assert_eq!(
        c.view(11999).step,
        Step::Retry {
            kind: Failure::Status,
            at: 12000
        }
    );
    assert!(!matches!(c.view(12000).step, Step::Retry { .. }));
    assert!(!matches!(c.view(12001).step, Step::Retry { .. }));
}
#[test]
fn documented_runtime_config_parses_and_preserves_repo_identity() {
    let docs = include_str!("../../../../../docs/conaryd.md");
    let json = docs
        .split("```json\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    let config: Runtime =
        serde_json::from_str(json).expect("the complete documented config must parse");
    let names = config
        .repos
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    assert_eq!(names, ["example/project"]);
    let value = serde_json::json!({"repository":{"full_name":"EXAMPLE/Project"}, "pull_request":{"number":3}});
    let allowed = names.into_iter().map(|r| r.to_lowercase()).collect();
    let kicks = crate::events::kicks("pull_request", &value, &allowed);
    assert_eq!(kicks.len(), 1);
    assert!(
        config
            .repos
            .iter()
            .any(|repo| repo.to_string().eq_ignore_ascii_case(&kicks[0].repo))
    );
}

#[test]
fn mailbox_unlocks_on_drop_even_with_an_inherited_descriptor() {
    let dir = tempfile::tempdir().unwrap();
    let queue = Queue::open(dir.path()).unwrap();
    let inherited = queue.duplicate_lock();
    assert!(
        Queue::open(dir.path()).is_err(),
        "a live owner must still exclude another worker"
    );
    queue.kick(kick()).unwrap();
    drop(queue);
    let restarted =
        Queue::open(dir.path()).expect("drop must unlock the shared description explicitly");
    assert!(
        restarted.take(1).unwrap().is_some(),
        "queued work survives the restart"
    );
    drop(inherited);
}
