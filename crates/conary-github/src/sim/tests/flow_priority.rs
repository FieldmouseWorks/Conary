//! Proof slots and the train take PRs by their priority level, then age,
//! then number, in full passes, dry runs and targeted kicks alike (#415).
use super::flow_pass_case::{Case, REPO};
use super::flow_pass_slots::{WAITING, another, kick, prepared, two};
use crate::flow::{self, *};
use serde_json::json;

fn opened(c: &Case, n: u64, at: &str) {
    c.hub.edit_pull(REPO, n, |p| p["created_at"] = json!(at));
}
fn close(c: &Case, n: u64) {
    c.hub.edit_pull(REPO, n, |p| p["state"] = json!("closed"));
}
/// Each report's PR and its decision's stage and step.
fn decided(reports: &[Report]) -> Vec<(u64, Stage, Step)> {
    reports
        .iter()
        .map(|r| {
            let next = r.next.as_ref().unwrap_or_else(|| panic!("{r:?}"));
            (r.pr, next.stage, next.step.clone())
        })
        .collect()
}
fn proving(reports: &[Report]) -> Vec<u64> {
    decided(reports)
        .into_iter()
        .filter(|(_, _, step)| *step == Step::Prove)
        .map(|(pr, _, _)| pr)
        .collect()
}
fn of(reports: &[Report], pr: u64) -> &Next {
    reports
        .iter()
        .find(|r| r.pr == pr)
        .and_then(|r| r.next.as_ref())
        .unwrap()
}

/// One proof slot and three waiting PRs: an old unlabelled one (3), a newer
/// `priority: high` one (7) and a newest `priority: low` one (5).
fn three() -> Case {
    let c = two(1);
    another(&c, 7);
    opened(&c, 3, "2099-01-01T00:00:00Z");
    opened(&c, 7, "2099-02-01T00:00:00Z");
    opened(&c, 5, "2099-03-01T00:00:00Z");
    c.hub.label(REPO, 7, &["priority: high"]);
    c.hub.label(REPO, 5, &["priority: low"]);
    c
}

#[test]
fn the_high_level_goes_first_then_the_old_one_then_the_low_level() {
    let c = three();
    let now = flow::jobs::now();
    prepared(&c, now, true);
    let dry = c.pass(now + 1, true);
    assert_eq!(proving(&dry), vec![7], "{:?}", decided(&dry));
    let real = c.pass(now + 1, false);
    assert_eq!(proving(&real), vec![7], "{:?}", decided(&real));
    assert_eq!(c.deferred.0.borrow().len(), 1);
    for waiting in [3, 5] {
        let next = of(&real, waiting);
        assert_eq!((next.stage, next.step.clone()), WAITING);
    }
    // The high one is overdue at its level's deadline, the others at theirs.
    assert_eq!(of(&real, 7).overdue_at, Some(now + 1 + 1800));
    assert_eq!(of(&real, 3).overdue_at, Some(now + 1 + 7200));
    // Its slot freed, the old unlabelled one goes, and the low one still waits.
    close(&c, 7);
    let real = c.pass(now + 2, false);
    assert_eq!(proving(&real), vec![3], "{:?}", decided(&real));
    close(&c, 3);
    let real = c.pass(now + 3, false);
    assert_eq!(proving(&real), vec![5], "{:?}", decided(&real));
}

#[test]
fn within_a_level_the_older_pr_goes_first_whatever_its_number() {
    let c = two(1);
    opened(&c, 5, "2098-12-01T00:00:00Z");
    let now = flow::jobs::now();
    prepared(&c, now, true);
    for dry in [true, false] {
        let reports = c.pass(now + 1, dry);
        assert_eq!(proving(&reports), vec![5], "{:?}", decided(&reports));
    }
}

#[test]
fn a_targeted_kick_waits_behind_a_higher_level_queued_on_another_pr() {
    let c = three();
    let now = flow::jobs::now();
    prepared(&c, now, true);
    for dry in [true, false] {
        let report = kick(&c, 3, dry, now + 1);
        assert_eq!(decided(&report), vec![(3, WAITING.0, WAITING.1)]);
    }
    assert!(c.deferred.0.borrow().is_empty());
}

#[test]
fn the_train_admits_the_high_level_ahead_of_an_older_pr() {
    let mut c = Case::new();
    c.config.settings.train = true;
    c.clean_review();
    another(&c, 5);
    for n in [3, 5] {
        c.hub.post_status(
            &super::head(n),
            json!({"context":"machine","state":"success"}),
        );
    }
    opened(&c, 5, "2099-02-01T00:00:00Z");
    c.hub.label(REPO, 5, &["priority: high"]);
    let ledger = tempfile::tempdir().unwrap();
    let lock = conary_core::log::Lock::take(ledger.path()).unwrap();
    let mut store =
        conary_core::Store::open(lock, ed25519_dalek::SigningKey::from_bytes(&[5; 32]), None)
            .unwrap();
    let reports = Pass {
        config: &c.config,
        policy: &c.policy,
        root: c.dir.path(),
        launcher: &c.deferred,
        now: flow::jobs::now(),
        dry_run: false,
        ledger: None,
    }
    .run(&c.hub, &REPO.parse().unwrap(), None, Some(&mut store))
    .unwrap();
    for report in &reports {
        assert_eq!(report.error, None, "{report:?}");
        assert_eq!(report.next.as_ref().unwrap().step, Step::JoinTrain);
    }
    let repo = REPO.parse().unwrap();
    let ledger = store.ledger();
    let joined: Vec<u64> = ledger
        .train(&repo)
        .unwrap()
        .joined()
        .iter()
        .map(|m| ledger.change(m.change).unwrap().pr)
        .collect();
    assert_eq!(joined, vec![5, 3]);
}
