//! Deferred checks against the simulated GitHub, across successive passes.
use super::flow_pass_case::{Case, REPO};
use super::head;
use crate::flow::*;
use crate::sim::Call;
use serde_json::json;

const HEAVY: &str = "heavy";

fn configured() -> Case {
    let mut c = Case::new();
    c.config.settings.deferred.checks.insert(
        HEAVY.into(),
        Start {
            workflow: "heavy.yml".into(),
            git_ref: None,
        },
    );
    c
}
fn step(c: &Case, now: u64) -> Step {
    let reports = c.pass(now, false);
    assert_eq!(reports[0].error, None);
    reports[0].next.as_ref().unwrap().step.clone()
}
fn runs(c: &Case) -> usize {
    c.hub.dispatched(REPO).len()
}
/// What the dispatched workflow does when it finishes: report on the PR's head.
fn report(c: &Case, head: &str, nonce: &str, conclusion: &str) -> u64 {
    let id = c.hub.run_id();
    c.hub.add_check(
        head,
        json!({"id":id,"name":HEAVY,"status":"completed","conclusion":conclusion,
            "external_id":nonce,"started_at":"2026-11-01T00:00:00Z"}),
    );
    if conclusion == "success" {
        c.attest_run(head, id, None);
    }
    id
}
fn nonce(c: &Case) -> String {
    c.record().dispatches.last().unwrap().nonce.clone()
}

#[test]
fn nothing_starts_before_review_is_ready() {
    let c = configured();
    let now = jobs::now();
    assert_eq!(step(&c, now), Step::QueueReview("reviewer".into()));
    // Cheap checks are not held back: the ordinary check is already green.
    assert_eq!(runs(&c), 0);
    assert_eq!(c.hub.count(Call::Dispatch), 0);
    assert!(c.record().dispatches.is_empty());
}

#[test]
fn a_clean_review_starts_the_deferred_check_once_across_passes() {
    let c = configured();
    c.clean_review();
    let now = jobs::now();
    assert_eq!(step(&c, now), Step::StartDeferred(vec![HEAVY.into()]));
    assert_eq!(runs(&c), 1);
    let sent = c.hub.dispatched(REPO)[0].clone();
    assert_eq!(sent["workflow"], "heavy.yml");
    assert_eq!(sent["ref"], "main");
    assert_eq!(sent["inputs"]["head"], head(3));
    assert_eq!(sent["inputs"]["pr"], "3");
    assert_eq!(sent["inputs"]["nonce"], nonce(&c));
    for later in 1..4 {
        assert!(matches!(step(&c, now + later), Step::Wait { .. }));
    }
    assert_eq!(runs(&c), 1);
    assert_eq!(
        c.record().dispatches[0].run,
        Some(c.hub.dispatched(REPO)[0]["id"].as_u64().unwrap())
    );
    // Never permission while the evidence is missing.
    assert_eq!(c.hub.count(Call::Land), 0);
}

#[test]
fn open_threads_keep_the_deferred_check_waiting() {
    let c = configured();
    c.clean_review();
    c.hub.edit_flow(REPO, 3, |s| s.open_threads = 1);
    let now = jobs::now();
    assert!(!matches!(step(&c, now), Step::StartDeferred(_)));
    assert_eq!(runs(&c), 0);
}

#[test]
fn it_starts_without_a_reviewer_configured() {
    let mut c = configured();
    c.config.settings.reviewers.clear();
    let now = jobs::now();
    assert_eq!(step(&c, now), Step::StartDeferred(vec![HEAVY.into()]));
    assert_eq!(runs(&c), 1);
    assert_eq!(c.hub.count(Call::Land), 0);
}

#[test]
fn nothing_is_configured_by_default() {
    let c = Case::new();
    c.clean_review();
    assert_eq!(step(&c, jobs::now()), Step::Merge);
    assert_eq!(c.hub.count(Call::Dispatch), 0);
}

#[test]
fn landing_needs_the_evidence_and_only_this_dispatchs_evidence() {
    let c = configured();
    c.clean_review();
    let now = jobs::now();
    step(&c, now);
    let n = nonce(&c);
    let run = c.hub.dispatched(REPO)[0]["id"].as_u64().unwrap();
    // A finished run with no check run reported is not a pass.
    c.hub.finish_workflow(REPO, run, "success");
    assert!(matches!(step(&c, now + 1), Step::Wait { .. }));
    // Nor is a same-named success that this dispatch did not make.
    report(&c, &head(3), "someone-else", "success");
    assert!(matches!(step(&c, now + 2), Step::Wait { .. }));
    assert_eq!(c.hub.count(Call::Land), 0);
    assert!(!c.hub.merged(REPO, 3).is_some());
    report(&c, &head(3), &n, "success");
    assert_eq!(step(&c, now + 3), Step::Merge);
    assert_eq!(c.hub.count(Call::Land), 1);
    assert_eq!(runs(&c), 1);
}

#[test]
fn permission_is_refused_without_evidence_even_when_asked_directly() {
    let c = configured();
    c.clean_review();
    let now = jobs::now();
    step(&c, now);
    let facts = |c: &Case| {
        let mut f = read(
            &c.hub,
            &REPO.parse().unwrap(),
            3,
            &c.policy,
            c.config.settings.clone(),
            &c.config.trusted_authors,
        )
        .unwrap();
        f.dispatches = c.record().dispatches;
        f
    };
    assert!(crate::flow::deferred::verify(&c.hub, &facts(&c), now + 1).is_err());
    report(&c, &head(3), &nonce(&c), "success");
    assert!(crate::flow::deferred::verify(&c.hub, &facts(&c), now + 1).is_ok());
}

#[test]
fn a_failed_deferred_check_goes_to_a_fix_not_to_landing() {
    let c = configured();
    c.clean_review();
    let now = jobs::now();
    step(&c, now);
    report(&c, &head(3), &nonce(&c), "failure");
    let next = step(&c, now + 1);
    assert_ne!(next, Step::Merge);
    assert_eq!(c.hub.count(Call::Land), 0);
}

#[test]
fn a_lost_dispatch_response_is_not_sent_again() {
    let c = configured();
    c.clean_review();
    c.hub.lose(Call::Dispatch);
    let now = jobs::now();
    let reports = c.pass(now, false);
    assert!(reports[0].error.is_some());
    assert_eq!(runs(&c), 1);
    c.hub.heal(Call::Dispatch);
    // The retry backoff, then the run found by its nonce.
    for later in [61, 130, 400, 800] {
        c.pass(now + later, false);
    }
    assert_eq!(runs(&c), 1);
    assert_eq!(c.record().dispatches.len(), 1);
    assert!(c.record().dispatches[0].run.is_some());
}

#[test]
fn a_dispatch_that_never_appears_is_sent_again_after_it_lapses() {
    let c = configured();
    c.clean_review();
    c.hub.fail(Call::Dispatch);
    let now = jobs::now();
    assert!(c.pass(now, false)[0].error.is_some());
    assert_eq!(runs(&c), 0);
    c.hub.heal(Call::Dispatch);
    c.pass(now + 61, false);
    assert_eq!(runs(&c), 0, "still within the time it may take to appear");
    let lapsed = now + 61 + c.config.settings.deferred.start_timeout + 1;
    let mut starts = 0;
    for later in 0..4 {
        if matches!(step(&c, lapsed + later * 200), Step::StartDeferred(_)) {
            starts += 1;
        }
    }
    assert_eq!(starts, 1);
    assert_eq!(runs(&c), 1);
    assert_eq!(c.record().dispatches.len(), 2);
}

#[test]
fn a_dispatch_gives_up_after_its_attempts() {
    let mut c = configured();
    c.config.settings.deferred.attempts = 1;
    c.clean_review();
    c.hub.fail(Call::Dispatch);
    let now = jobs::now();
    c.pass(now, false);
    c.hub.heal(Call::Dispatch);
    let lapsed = now + c.config.settings.deferred.start_timeout + 100;
    // The failure's own retry pass, then the wait for what cannot be sent.
    c.pass(lapsed, false);
    for later in 1..4 {
        assert!(matches!(step(&c, lapsed + later * 100), Step::Wait { .. }));
    }
    assert_eq!(runs(&c), 0);
    assert_eq!(c.record().dispatches.len(), 1);
}

#[test]
fn a_push_during_the_dispatch_cancels_the_run_for_the_old_head() {
    let c = configured();
    c.clean_review();
    c.hub.race(Call::Dispatch, |hub| {
        hub.push(REPO, 3, &head(4));
        hub.refresh_flow(REPO, 3);
    });
    let now = jobs::now();
    step(&c, now);
    let sent = c.hub.dispatched(REPO)[0].clone();
    assert_eq!(sent["inputs"]["head"], head(3));
    c.good_ci(&head(4));
    // The next pass sees the run for an earlier head and cancels it first.
    let n = nonce(&c);
    assert_eq!(step(&c, now + 1), Step::CancelDeferred(n.clone()));
    assert_eq!(c.hub.dispatched(REPO)[0]["conclusion"], "cancelled");
    // Its late success is for the old head and counts for nothing here.
    report(&c, &head(3), &n, "success");
    step(&c, now + 2);
    assert!(c.record().dispatches.iter().all(|d| d.head != head(3)));
    assert_eq!(c.hub.count(Call::Land), 0);
}

#[test]
fn a_fix_push_cancels_running_work_and_the_new_head_starts_its_own() {
    let c = configured();
    c.clean_review();
    let now = jobs::now();
    step(&c, now);
    let old = nonce(&c);
    c.hub.push(REPO, 3, &head(4));
    c.hub.refresh_flow(REPO, 3);
    c.good_ci(&head(4));
    assert_eq!(step(&c, now + 1), Step::CancelDeferred(old.clone()));
    assert_eq!(c.hub.count(Call::CancelRun), 1);
    // Cancelling is not repeated once done; the record forgets the old head.
    step(&c, now + 2);
    assert_eq!(c.hub.count(Call::CancelRun), 1);
    assert!(c.record().dispatches.is_empty());
}

#[test]
fn a_failed_cancel_is_asked_again() {
    let c = configured();
    c.clean_review();
    let now = jobs::now();
    step(&c, now);
    c.hub.push(REPO, 3, &head(4));
    c.hub.refresh_flow(REPO, 3);
    c.good_ci(&head(4));
    c.hub.fail(Call::CancelRun);
    assert!(c.pass(now + 1, false)[0].error.is_some());
    assert_eq!(c.hub.dispatched(REPO)[0]["status"], "queued");
    c.hub.heal(Call::CancelRun);
    for later in [100, 5000] {
        c.pass(now + later, false);
    }
    assert_eq!(c.hub.dispatched(REPO)[0]["conclusion"], "cancelled");
}

#[test]
fn dry_runs_neither_dispatch_nor_save() {
    let c = configured();
    c.clean_review();
    let reports = c.pass(jobs::now(), true);
    assert_eq!(
        reports[0].next.as_ref().unwrap().step,
        Step::StartDeferred(vec![HEAVY.into()])
    );
    assert_eq!(c.hub.count(Call::Dispatch), 0);
    assert_eq!(std::fs::read_dir(c.dir.path()).unwrap().count(), 0);
}

#[test]
fn a_configuration_that_collides_with_a_gate_is_refused() {
    let mut c = configured();
    c.config.settings.deferred.checks.insert(
        "negative".into(),
        Start {
            workflow: "heavy.yml".into(),
            git_ref: None,
        },
    );
    let mut store = c.store.borrow_mut();
    let result = Pass {
        config: &c.config,
        policy: &c.policy,
        root: c.dir.path(),
        launcher: &c.deferred,
        now: jobs::now(),
        dry_run: false,
        ledger: None,
    }
    .run(&c.hub, &REPO.parse().unwrap(), None, Some(&mut store));
    assert!(result.is_err());
}
