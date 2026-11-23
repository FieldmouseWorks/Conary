//! The flow's off switches reach deferred runs too: a pause or a hold stops
//! the PR's owned runs, nothing new is dispatched while it lasts, and a stop
//! it asked for spends no attempt. A closed PR's runs are stopped as well.
use super::*;
use crate::flow::pause::{Pause, Scope};

fn pause(c: &Case) {
    let paused = Pause {
        scope: Scope::Repository,
        reason: "maintenance".into(),
        at: 0,
    };
    crate::flow::pause::pause(c.dir.path(), &REPO.parse().unwrap(), &paused).unwrap();
}
fn resume(c: &Case) {
    crate::flow::pause::resume(c.dir.path(), &REPO.parse().unwrap()).unwrap();
}
fn hold(c: &Case) {
    c.hub
        .label(REPO, 3, &[&c.config.settings.hold_label.clone()]);
}
fn unhold(c: &Case) {
    c.hub.label(REPO, 3, &[]);
}
/// A step that may stop for the pause, but for nothing else.
fn quiet_step(c: &Case, now: u64) -> Step {
    let reports = c.pass(now, false);
    assert!(
        reports[0]
            .error
            .as_deref()
            .is_none_or(|e| e.contains("paused")),
        "{:?}",
        reports[0].error
    );
    reports[0].next.as_ref().unwrap().step.clone()
}
fn status(c: &Case, run: u64) -> (String, Option<String>) {
    let r = c
        .hub
        .dispatched(REPO)
        .into_iter()
        .find(|r| r["id"] == run)
        .unwrap();
    (
        r["status"].as_str().unwrap().into(),
        r["conclusion"].as_str().map(Into::into),
    )
}

#[test]
fn a_pause_or_hold_stops_a_running_deferred_check() {
    for (off, on) in [
        (pause as fn(&Case), resume as fn(&Case)),
        (hold as fn(&Case), unhold as fn(&Case)),
    ] {
        let c = configured();
        c.clean_review();
        let now = jobs::now();
        assert_eq!(step(&c, now), Step::StartDeferred(vec![HEAVY.into()]));
        let n = nonce(&c);
        let run = run_of(&c, &n).unwrap();
        off(&c);
        assert_eq!(quiet_step(&c, now + 1), Step::CancelDeferred(n.clone()));
        assert_eq!(status(&c, run).1.as_deref(), Some("cancelled"));
        // Nothing is dispatched while it lasts.
        for later in [2, 100, 2000] {
            let next = quiet_step(&c, now + later);
            assert!(!matches!(next, Step::StartDeferred(_)), "{next:?}");
        }
        assert_eq!(c.hub.count(Call::Dispatch), 1);
        on(&c);
        // Lifted, the check starts again from nothing.
        assert_eq!(
            step(&c, now + 2001),
            Step::StartDeferred(vec![HEAVY.into()])
        );
        assert_eq!(c.hub.count(Call::Dispatch), 2);
    }
}

#[test]
fn a_stop_the_pause_asked_for_spends_no_attempt() {
    let mut c = configured();
    c.config.settings.deferred.attempts = 1;
    c.clean_review();
    let mut now = jobs::now();
    for round in 1..=3 {
        assert_eq!(step(&c, now), Step::StartDeferred(vec![HEAVY.into()]));
        pause(&c);
        assert!(matches!(quiet_step(&c, now + 1), Step::CancelDeferred(_)));
        quiet_step(&c, now + 2);
        resume(&c);
        assert_eq!(c.hub.count(Call::Dispatch), round);
        now += 10;
    }
    // A run lost on its own still spends its attempt.
    step(&c, now);
    let lost = now + c.config.settings.deferred.start_timeout + 1;
    c.hub
        .finish_workflow(REPO, run_of(&c, &nonce(&c)).unwrap(), "cancelled");
    let next = step(&c, lost);
    assert!(!matches!(next, Step::StartDeferred(_)), "{next:?}");
}

#[test]
fn an_ignored_stop_is_asked_again_while_paused() {
    let c = configured();
    c.clean_review();
    let now = jobs::now();
    step(&c, now);
    let n = nonce(&c);
    let run = run_of(&c, &n).unwrap();
    c.hub.ignore_cancels(true);
    pause(&c);
    assert_eq!(quiet_step(&c, now + 1), Step::CancelDeferred(n.clone()));
    // Asked, still going: the pass books its next ask, not the run timeout.
    let reports = c.pass(now + 2, false);
    let until = reports[0].next.as_ref().unwrap().wait_until;
    assert!(until.is_some_and(|t| t <= now + 1 + 61), "{until:?}");
    assert_eq!(quiet_step(&c, now + 70), Step::CancelDeferred(n.clone()));
    assert!(c.hub.count(Call::CancelRun) >= 2);
    c.hub.ignore_cancels(false);
    quiet_step(&c, now + 140);
    assert_eq!(status(&c, run).1.as_deref(), Some("cancelled"));
    assert_eq!(c.hub.count(Call::Dispatch), 1);
}

#[test]
fn a_run_first_seen_during_a_pause_is_stopped() {
    let c = configured();
    c.clean_review();
    c.hub.cap_run_list(Some(0));
    let now = jobs::now();
    step(&c, now);
    let n = nonce(&c);
    pause(&c);
    quiet_step(&c, now + 1);
    assert_eq!(c.hub.count(Call::CancelRun), 0);
    // It shows up while the pause lasts.
    c.hub.cap_run_list(None);
    assert_eq!(quiet_step(&c, now + 2), Step::CancelDeferred(n.clone()));
    assert_eq!(
        status(&c, run_of(&c, &n).unwrap()).1.as_deref(),
        Some("cancelled")
    );
}

#[test]
fn an_obsolete_run_is_stopped_while_paused() {
    let c = configured();
    c.clean_review();
    let now = jobs::now();
    step(&c, now);
    let old = nonce(&c);
    c.hub.push(REPO, 3, &head(4));
    c.hub.refresh_flow(REPO, 3);
    c.good_ci(&head(4));
    pause(&c);
    assert_eq!(quiet_step(&c, now + 1), Step::CancelDeferred(old.clone()));
    quiet_step(&c, now + 2);
    assert!(c.record().dispatches.is_empty());
}

#[test]
fn what_finished_despite_the_stop_is_kept() {
    let c = configured();
    c.clean_review();
    let now = jobs::now();
    step(&c, now);
    let n = nonce(&c);
    c.hub.ignore_cancels(true);
    pause(&c);
    assert_eq!(quiet_step(&c, now + 1), Step::CancelDeferred(n.clone()));
    // GitHub ignored the stop and the run passed: its evidence stays.
    report(&c, &head(3), &n, "success");
    quiet_step(&c, now + 2);
    resume(&c);
    c.hub.ignore_cancels(false);
    let next = step(&c, now + 3);
    assert!(!matches!(next, Step::StartDeferred(_)), "{next:?}");
    assert_eq!(c.hub.count(Call::Dispatch), 1);
    assert!(c.record().dispatches.iter().any(|d| d.nonce == n));
}

#[test]
fn a_closed_prs_running_check_is_stopped_and_forgotten() {
    let c = configured();
    c.clean_review();
    let now = jobs::now();
    step(&c, now);
    let n = nonce(&c);
    let run = run_of(&c, &n).unwrap();
    c.hub
        .edit_pull(REPO, 3, |p| p["state"] = serde_json::json!("closed"));
    c.pass(now + 1, false);
    assert_eq!(c.hub.count(Call::CancelRun), 1);
    assert_eq!(status(&c, run).1.as_deref(), Some("cancelled"));
    c.pass(now + 2, false);
    assert!(c.record().dispatches.is_empty());
    assert_eq!(c.hub.count(Call::CancelRun), 1);
}

#[test]
fn a_rejected_stop_is_asked_again_while_paused() {
    let c = configured();
    c.clean_review();
    let now = jobs::now();
    step(&c, now);
    let n = nonce(&c);
    let run = run_of(&c, &n).unwrap();
    pause(&c);
    c.hub.fail(Call::CancelRun);
    assert!(c.pass(now + 1, false)[0].error.is_some());
    assert_eq!(status(&c, run).0, "queued");
    c.hub.heal(Call::CancelRun);
    assert_eq!(quiet_step(&c, now + 62), Step::CancelDeferred(n.clone()));
    assert_eq!(status(&c, run).1.as_deref(), Some("cancelled"));
}

#[test]
fn a_closed_prs_ignored_stop_is_asked_again_and_an_unseen_run_is_forgotten() {
    let c = configured();
    c.clean_review();
    let now = jobs::now();
    step(&c, now);
    c.hub.ignore_cancels(true);
    c.hub
        .edit_pull(REPO, 3, |p| p["state"] = serde_json::json!("closed"));
    c.pass(now + 1, false);
    c.pass(now + 2, false);
    assert_eq!(c.hub.count(Call::CancelRun), 1);
    c.pass(now + 62, false);
    assert_eq!(c.hub.count(Call::CancelRun), 2);

    // A dispatch that never shows: looked for until its start timeout.
    let c = configured();
    c.clean_review();
    c.hub.cap_run_list(Some(0));
    step(&c, now);
    c.hub
        .edit_pull(REPO, 3, |p| p["state"] = serde_json::json!("closed"));
    c.pass(now + 1, false);
    assert_eq!(c.record().dispatches.len(), 1);
    c.pass(now + c.config.settings.deferred.start_timeout + 1, false);
    assert!(c.record().dispatches.is_empty());
    assert_eq!(c.hub.count(Call::CancelRun), 0);
}
