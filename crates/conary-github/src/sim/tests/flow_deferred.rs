//! Deferred checks against the simulated GitHub, across successive passes.
use super::flow_pass_case::{Case, REPO};
use super::head;
use crate::attest::testing;
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
/// The dispatched run owning `nonce`, if any.
fn run_of(c: &Case, nonce: &str) -> Option<u64> {
    let title = format!("conary {nonce}");
    c.hub
        .dispatched(REPO)
        .iter()
        .find(|r| r["display_title"] == title)
        .map(|r| r["id"].as_u64().unwrap())
}
/// Signs a dispatched job's statement as GitHub does for a dispatch: the token
/// is for the default branch's commit, the statement names the head.
fn sign(c: &Case, head: &str, job: u64, nonce: &str) {
    sign_for(c, head, job, nonce, "workflow_dispatch");
}
fn sign_for(c: &Case, head: &str, job: u64, nonce: &str, event: &str) {
    let tree = crate::branches::Branches::tree(&c.hub, REPO, head).unwrap();
    let statement = testing::deferred_statement(head, &tree, nonce, "pass");
    let mut claims = testing::claims(REPO, &"9".repeat(40), job, &statement);
    claims["event_name"] = json!(event);
    c.hub
        .attest(job, &testing::file(&statement, &testing::token(&claims)));
}
/// What the dispatched workflow does when it finishes: its job reports on the
/// PR's head. A nonce nobody dispatched is a job of some other run.
fn report(c: &Case, head: &str, nonce: &str, conclusion: &str) -> u64 {
    let owned = run_of(c, nonce);
    let id = owned.map_or_else(|| c.hub.run_id(), |run| run / 10);
    c.hub.add_check(
        head,
        json!({"id":id,"name":HEAVY,"status":"completed","conclusion":conclusion,
            "started_at":"2026-11-01T00:00:00Z"}),
    );
    if let Some(run) = owned {
        c.hub.finish_workflow(REPO, run, conclusion);
    }
    if conclusion == "success" {
        sign(c, head, id, nonce);
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

/// An owned job that succeeded without anyone signing it.
fn unsigned(c: &Case, head: &str, nonce: &str) -> u64 {
    let run = run_of(c, nonce).unwrap();
    let id = run / 10;
    c.hub.add_check(
        head,
        json!({"id":id,"name":HEAVY,"status":"completed","conclusion":"success",
            "started_at":"2026-11-01T00:00:00Z"}),
    );
    c.hub.finish_workflow(REPO, run, "success");
    id
}
fn refused(c: &Case, now: u64) {
    let reports = c.pass(now, false);
    assert!(
        reports[0].error.is_some()
            || !matches!(reports[0].next.as_ref().unwrap().step, Step::Merge),
        "{:?}",
        reports[0].next
    );
    assert_eq!(c.hub.count(Call::Land), 0);
    assert!(c.hub.merged(REPO, 3).is_none());
}

#[test]
fn a_foreign_attested_run_never_supplies_the_owned_runs_missing_attestation() {
    let c = configured();
    c.clean_review();
    let now = jobs::now();
    step(&c, now);
    let n = nonce(&c);
    // The owned job succeeded but nobody attested it; a newer same-named job
    // of some other run is attested, even naming this nonce.
    unsigned(&c, &head(3), &n);
    let foreign = report(&c, &head(3), "someone-else", "success");
    sign(&c, &head(3), foreign, &n);
    refused(&c, now + 1);
    refused(&c, now + 2);
}

#[test]
fn the_signed_statement_must_name_this_dispatch_and_a_dispatch_event() {
    let c = configured();
    c.clean_review();
    let now = jobs::now();
    step(&c, now);
    let n = nonce(&c);
    let id = unsigned(&c, &head(3), &n);
    sign(&c, &head(3), id, "another-dispatch");
    refused(&c, now + 1);
    c.hub.forget_artifacts(Some(id));
    sign_for(&c, &head(3), id, &n, "push");
    refused(&c, now + 2);
    c.hub.forget_artifacts(Some(id));
    sign(&c, &head(3), id, &n);
    assert_eq!(step(&c, now + 3), Step::Merge);
    assert_eq!(c.hub.count(Call::Land), 1);
}

#[test]
fn a_dispatch_from_the_default_branch_is_accepted_on_the_prs_head() {
    // The token's own commit is the default branch's, never the PR head's.
    let c = configured();
    c.clean_review();
    let now = jobs::now();
    step(&c, now);
    report(&c, &head(3), &nonce(&c), "success");
    assert_ne!(head(3), "9".repeat(40));
    assert_eq!(step(&c, now + 1), Step::Merge);
}

#[test]
fn an_accepted_dispatch_that_never_appears_is_woken_at_its_start_timeout() {
    for reviewers in [true, false] {
        let mut c = configured();
        if reviewers {
            c.clean_review();
        } else {
            c.config.settings.reviewers.clear();
        }
        // Accepted, but no run ever shows in the list.
        c.hub.cap_run_list(Some(0));
        let now = jobs::now();
        let due = now + c.config.settings.deferred.start_timeout;
        for later in [0, 5, 10] {
            let reports = c.pass(now + later, false);
            assert_eq!(reports[0].next.as_ref().unwrap().wait_until, Some(due));
        }
        // At the deadline it is lost and asked for again, without a webhook.
        assert!(matches!(step(&c, due + 1), Step::StartDeferred(_)));
    }
}

#[test]
fn a_running_check_books_its_run_timeout() {
    let c = configured();
    c.clean_review();
    let now = jobs::now();
    step(&c, now);
    let reports = c.pass(now + 1, false);
    assert_eq!(
        reports[0].next.as_ref().unwrap().wait_until,
        Some(now + c.config.settings.deferred.run_timeout)
    );
}

#[test]
fn a_run_past_the_first_page_of_runs_is_still_found_by_its_id() {
    let c = configured();
    c.clean_review();
    let now = jobs::now();
    step(&c, now);
    let old = nonce(&c);
    let run = run_of(&c, &old).unwrap();
    step(&c, now + 1);
    // Newer dispatches push it off the list that is read.
    for k in 0..3 {
        crate::flow::Write::dispatch(
            &c.hub,
            REPO,
            "heavy.yml",
            "main",
            &[("nonce".to_string(), format!("noise-{k}"))].into(),
        )
        .unwrap();
    }
    c.hub.cap_run_list(Some(2));
    c.hub.push(REPO, 3, &head(4));
    c.hub.refresh_flow(REPO, 3);
    c.good_ci(&head(4));
    assert_eq!(step(&c, now + 2), Step::CancelDeferred(old.clone()));
    assert_eq!(
        c.hub
            .dispatched(REPO)
            .iter()
            .find(|r| r["id"] == run)
            .unwrap()["conclusion"],
        "cancelled"
    );
    // Nor does its absence from the list count it as lost and send it again.
    assert_eq!(c.hub.count(Call::Dispatch), 4);
}

#[test]
fn a_cancel_request_is_not_a_stop() {
    let c = configured();
    c.clean_review();
    let now = jobs::now();
    step(&c, now);
    let old = nonce(&c);
    c.hub.push(REPO, 3, &head(4));
    c.hub.refresh_flow(REPO, 3);
    c.good_ci(&head(4));
    c.hub.ignore_cancels(true);
    assert_eq!(step(&c, now + 1), Step::CancelDeferred(old.clone()));
    // GitHub accepted it, the run still goes: it is kept, asked again, and
    // nothing replaces it.
    assert!(c.record().dispatches.iter().any(|d| d.nonce == old));
    for later in [2, 70, 140, 210] {
        let next = step(&c, now + later);
        assert!(!matches!(next, Step::StartDeferred(_)), "{next:?}");
    }
    assert!(c.hub.count(Call::CancelRun) >= 3);
    assert_eq!(c.hub.count(Call::Dispatch), 1);
    assert!(c.record().dispatches.iter().any(|d| d.nonce == old));
    c.hub.ignore_cancels(false);
    step(&c, now + 300);
    step(&c, now + 301);
    assert!(c.record().dispatches.iter().all(|d| d.nonce != old));
}

#[test]
fn a_timed_out_run_keeps_being_stopped_and_is_not_replaced_while_it_runs() {
    let c = configured();
    c.clean_review();
    let now = jobs::now();
    step(&c, now);
    let old = nonce(&c);
    c.hub.ignore_cancels(true);
    let late = now + c.config.settings.deferred.run_timeout + 1;
    assert_eq!(step(&c, late), Step::CancelDeferred(old.clone()));
    for later in [1, 70, 140] {
        let next = step(&c, late + later);
        assert!(!matches!(next, Step::StartDeferred(_)), "{next:?}");
    }
    assert!(c.hub.count(Call::CancelRun) >= 2);
    assert_eq!(c.hub.count(Call::Dispatch), 1);
    c.hub.ignore_cancels(false);
    step(&c, late + 300);
    assert!(matches!(
        step(&c, late + 301),
        Step::StartDeferred(_) | Step::Wait { .. }
    ));
}

#[test]
fn a_run_is_cleaned_up_after_its_check_is_repointed_or_removed() {
    for remove in [false, true] {
        let mut c = configured();
        c.clean_review();
        let now = jobs::now();
        step(&c, now);
        let old = nonce(&c);
        let run = run_of(&c, &old).unwrap();
        if remove {
            c.config.settings.deferred.checks.clear();
        } else {
            c.config
                .settings
                .deferred
                .checks
                .get_mut(HEAVY)
                .unwrap()
                .workflow = "other.yml".into();
        }
        assert_eq!(step(&c, now + 1), Step::CancelDeferred(old.clone()));
        assert_eq!(
            c.hub
                .dispatched(REPO)
                .iter()
                .find(|r| r["id"] == run)
                .unwrap()["conclusion"],
            "cancelled"
        );
        step(&c, now + 2);
        assert!(c.record().dispatches.iter().all(|d| d.nonce != old));
    }
}

mod capped {
    use super::super::flow_case::Case;
    use crate::flow::deferred::Deferral;
    use crate::flow::*;

    fn five() -> Case {
        let mut c = Case::new();
        c.roles(&["primary", "secondary"]);
        for t in 1..=5 {
            c.review("primary", Verdict::Findings, t);
        }
        c.answered("primary");
        c
    }
    fn view(c: &Case, state: Deferral, now: u64) -> Next {
        let mut f = c.facts();
        f.deferred.insert("heavy".into(), state);
        next(&f, &c.memory, now).0
    }

    #[test]
    fn a_capped_review_is_judged_before_it_waits_for_evidence() {
        let mut c = five();
        let start = |c: &Case| view(c, Deferral::Unstarted, 4000);
        let wait = Deferral::InFlight { wake: 9999 };
        // Not ready, and nothing to wait on yet: the judge is asked.
        assert_eq!(start(&c).step, Step::Judge);
        assert_eq!(view(&c, wait, 4000).step, Step::Judge);
        c.judge("land");
        assert_eq!(start(&c).step, Step::StartDeferred(vec!["heavy".into()]));
        assert!(matches!(view(&c, wait, 4001).step, Step::Wait { .. }));
        let done = Deferral::Done {
            run: 1,
            check_run: 1,
        };
        assert_eq!(view(&c, done, 4002).step, Step::Merge);
    }

    #[test]
    fn an_approved_owner_judgment_is_readiness_too() {
        let mut c = five();
        c.judge("owner");
        assert_eq!(view(&c, Deferral::Unstarted, 4000).step, Step::ListOwner);
        c.approve();
        assert_eq!(
            view(&c, Deferral::Unstarted, 4001).step,
            Step::StartDeferred(vec!["heavy".into()])
        );
    }

    #[test]
    fn a_push_invalidates_the_judgment_and_it_is_asked_again() {
        let mut c = five();
        c.judge("land");
        c.push(&super::head(4));
        assert_eq!(view(&c, Deferral::Unstarted, 4000).step, Step::Judge);
    }
}
