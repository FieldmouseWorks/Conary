use super::*;
use conary_core::plugin::{Problem, Task};

#[test]
fn owned_failures_reach_a_scoped_fix_with_run_diagnostics() {
    for conclusion in ["failure", "timed_out", "action_required"] {
        let c = configured();
        c.clean_review();
        let now = jobs::now();
        step(&c, now);
        let id = report(&c, &head(3), &nonce(&c), conclusion);
        assert_eq!(step(&c, now + 1), Step::HandFix(Kind::Red));
        let paths = c.deferred.0.borrow();
        let invocation: jobs::Invocation =
            serde_json::from_slice(&std::fs::read(paths.last().unwrap()).unwrap()).unwrap();
        let Task::Fix {
            head: tested,
            problems,
            ..
        } = invocation.task
        else {
            panic!("expected fix")
        };
        assert_eq!(tested, head(3));
        assert!(problems.iter().any(|p| matches!(p, Problem::RedCheck { check, text }
            if check == HEAVY && text.contains(conclusion) && text.contains(&(id * 10).to_string()))));
        assert_eq!(c.hub.count(Call::Dispatch), 1);
        assert_eq!(c.hub.count(Call::Land), 0);
    }
}

#[test]
fn retryable_terminal_outcomes_wait_until_execution_is_eligible() {
    for conclusion in [
        "cancelled",
        "skipped",
        "neutral",
        "startup_failure",
        "success",
    ] {
        let c = configured();
        c.clean_review();
        let now = jobs::now();
        step(&c, now);
        let run = run_of(&c, &nonce(&c)).unwrap();
        c.hub.finish_workflow(REPO, run, conclusion);
        let due = now + c.config.settings.deferred.start_timeout;
        let reports = c.pass(now + 1, false);
        assert_eq!(reports[0].error, None);
        assert!(matches!(
            reports[0].next.as_ref().unwrap().step,
            Step::Wait { .. }
        ));
        assert_eq!(
            reports[0].next.as_ref().unwrap().wait_until,
            Some(due),
            "{conclusion}"
        );
        assert_eq!(step(&c, due), Step::StartDeferred(vec![HEAVY.into()]));
        assert_eq!(c.hub.count(Call::Dispatch), 2);
    }
}

#[test]
fn obsolete_and_foreign_failures_never_reach_a_fix() {
    for obsolete in [false, true] {
        let mut c = configured();
        c.config.settings.reviewers.clear();
        let now = jobs::now();
        step(&c, now);
        if obsolete {
            report(&c, &head(3), &nonce(&c), "failure");
            c.hub.push(REPO, 3, &head(4));
            c.hub.refresh_flow(REPO, 3);
            c.good_ci(&head(4));
        } else {
            // This foreign job shares the dispatch ref's commit and name.
            report(&c, &"9".repeat(40), "foreign", "failure");
        }
        assert_ne!(step(&c, now + 1), Step::HandFix(Kind::Red));
    }
}

#[test]
fn proof_read_errors_do_not_spend_another_dispatch_attempt() {
    for call in [
        Call::Job,
        Call::Tree,
        Call::Artifacts,
        Call::Artifact,
        Call::Keys,
        Call::DefaultBranch,
        Call::Branch,
        Call::MergeBase,
    ] {
        let c = configured();
        c.clean_review();
        let now = jobs::now();
        step(&c, now);
        report(&c, &head(3), &nonce(&c), "success");
        // Force the signer history lookup, rather than a tip equality shortcut.
        if call == Call::MergeBase {
            // The signer branch's tip may move after the signing job.
            c.hub.set_branch("Acme/ci", "main", &head(8));
        }
        // The initial ownership lookup succeeds; the failure occurs inside
        // proof verification, where it must not be flattened to invalid proof.
        c.hub.race(Call::Job, move |h| {
            if call == Call::Job {
                h.race(Call::Job, |h| h.race(Call::Job, |h| h.fail(Call::Job)));
            } else {
                h.fail(call);
            }
        });
        let reports = c.pass(now + c.config.settings.deferred.start_timeout + 1, false);
        assert!(reports[0].error.is_some(), "{call:?}: {reports:?}");
        assert_eq!(c.hub.count(Call::Dispatch), 1, "{call:?}");
        assert_eq!(c.record().dispatches.len(), 1);
        assert_eq!(c.hub.count(Call::Land), 0);
    }
}

fn two() -> Case {
    let mut c = configured();
    c.config.settings.deferred.run_timeout = 100;
    c.config.settings.deferred.checks.insert(
        "heavy2".into(),
        Start {
            workflow: "heavy2.yml".into(),
            git_ref: None,
        },
    );
    c.clean_review();
    c
}

#[test]
fn failed_cancellations_do_not_starve_other_runs_or_poll_during_backoff() {
    for lost_response in [false, true] {
        let c = two();
        let now = jobs::now();
        step(&c, now);
        c.hub.ignore_cancels(true);
        if lost_response {
            c.hub.lose(Call::CancelRun);
        } else {
            c.hub.fail(Call::CancelRun);
        }
        let due = now + 101;
        let first = c.pass(due, false);
        assert!(first[0].error.is_some());
        let second = c.pass(due + 1, false);
        assert!(second[0].error.is_some());
        assert_eq!(c.hub.count(Call::CancelRun), 2);
        let reports = c.pass(due + 2, false);
        assert_eq!(reports[0].next.as_ref().unwrap().wait_until, Some(due + 60));
        assert_eq!(c.hub.count(Call::CancelRun), 2);
        assert_eq!(c.record().dispatches.len(), 2);
        c.hub.heal(Call::CancelRun);
        c.hub.ignore_cancels(false);
        c.pass(due + 60, false);
        c.pass(due + 61, false);
        assert!(
            c.hub
                .dispatched(REPO)
                .iter()
                .all(|r| r["status"] == "completed")
        );
    }
}

#[test]
fn unseen_obsolete_dispatches_are_retained_and_late_runs_are_stopped() {
    for change in ["head", "workflow", "ref", "default", "remove"] {
        let mut c = configured();
        c.config.settings.reviewers.clear();
        let now = jobs::now();
        c.hub.cap_run_list(Some(0));
        step(&c, now);
        let old = nonce(&c);
        match change {
            "head" => {
                c.hub.push(REPO, 3, &head(4));
                c.hub.refresh_flow(REPO, 3);
                c.good_ci(&head(4));
            }
            "workflow" => {
                c.config
                    .settings
                    .deferred
                    .checks
                    .get_mut(HEAVY)
                    .unwrap()
                    .workflow = "other.yml".into()
            }
            "ref" => {
                c.config
                    .settings
                    .deferred
                    .checks
                    .get_mut(HEAVY)
                    .unwrap()
                    .git_ref = Some("other".into())
            }
            "default" => c.hub.set_default(REPO, "other"),
            "remove" => c.config.settings.deferred.checks.clear(),
            _ => unreachable!(),
        }
        let late = now + c.config.settings.deferred.start_timeout + 1;
        let reports = c.pass(late, false);
        assert_eq!(reports[0].error, None, "{change}");
        assert!(
            c.record().dispatches.iter().any(|d| d.nonce == old),
            "{change}"
        );
        assert!(
            reports[0]
                .next
                .as_ref()
                .unwrap()
                .wait_until
                .is_some_and(|t| t > late && t <= late + 60)
        );
        assert_eq!(runs(&c), 1);
        // Listing catches up after an outage, without another head/config change.
        c.hub.cap_run_list(None);
        assert_eq!(
            step(&c, late + 60),
            Step::CancelDeferred(old.clone()),
            "{change}"
        );
        step(&c, late + 61);
        assert!(!c.record().dispatches.iter().any(|d| d.nonce == old));
    }
}

#[test]
fn authorization_preserves_proof_read_errors_for_event_backoff() {
    for after_observation in [false, true] {
        let c = configured();
        c.clean_review();
        let now = jobs::now();
        step(&c, now);
        report(&c, &head(3), &nonce(&c), "success");
        if after_observation {
            // Observation succeeds; the independent authorization read fails.
            c.hub.race(Call::Artifacts, |h| {
                h.race(Call::Artifacts, |h| h.fail(Call::Artifacts))
            });
            let reports = c.pass(now + 1, false);
            assert!(
                reports[0]
                    .error
                    .as_ref()
                    .is_some_and(|e| e.contains("Server Error")),
                "{reports:?}"
            );
        } else {
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
            c.hub.fail(Call::Artifacts);
            let pass = Pass {
                config: &c.config,
                policy: &c.policy,
                root: c.dir.path(),
                launcher: &c.deferred,
                now: now + 1,
                dry_run: false,
                ledger: None,
            };
            let error = crate::flow::evidence::authorize(&pass, &c.hub, &f).unwrap_err();
            assert!(error.downcast_ref::<crate::ApiError>().is_some(), "{error}");
        }
        assert_eq!(c.hub.count(Call::Dispatch), 1);
        assert_eq!(c.hub.count(Call::Land), 0);
    }
}

#[test]
fn invalid_missing_expired_and_wrong_scope_proofs_have_bounded_recovery() {
    for kind in ["missing", "invalid", "expired", "head", "tree", "nonce"] {
        let c = configured();
        c.clean_review();
        let now = jobs::now();
        step(&c, now);
        let n = nonce(&c);
        let id = unsigned(&c, &head(3), &n);
        match kind {
            "missing" => {}
            "invalid" => c.hub.attest(id, "invalid attestation"),
            "expired" => {
                sign(&c, &head(3), id, &n);
                c.hub.world.borrow_mut().expired_artifacts.insert(id);
            }
            "head" => {
                let tree = crate::branches::Branches::tree(&c.hub, REPO, &head(3)).unwrap();
                let statement = testing::deferred_statement(&head(4), &tree, &n, "pass");
                let mut claims = testing::claims(REPO, &"9".repeat(40), id, &statement);
                claims["event_name"] = json!("workflow_dispatch");
                c.hub
                    .attest(id, &testing::file(&statement, &testing::token(&claims)));
            }
            "nonce" => sign(&c, &head(3), id, "wrong-nonce"),
            "tree" => {
                let statement = testing::deferred_statement(&head(3), &"0".repeat(40), &n, "pass");
                let mut claims = testing::claims(REPO, &"9".repeat(40), id, &statement);
                claims["event_name"] = json!("workflow_dispatch");
                c.hub
                    .attest(id, &testing::file(&statement, &testing::token(&claims)));
            }
            _ => unreachable!(),
        }
        let reports = c.pass(now + 1, false);
        assert_eq!(reports[0].error, None, "{kind}");
        assert_eq!(
            reports[0].next.as_ref().unwrap().wait_until,
            Some(now + c.config.settings.deferred.start_timeout),
            "{kind}"
        );
        assert_eq!(
            step(&c, now + c.config.settings.deferred.start_timeout),
            Step::StartDeferred(vec![HEAVY.into()])
        );
        assert_eq!(c.hub.count(Call::Land), 0);
    }
}

#[test]
fn a_changed_owned_failure_is_rejected_by_the_scope_reread() {
    let c = configured();
    c.clean_review();
    let now = jobs::now();
    step(&c, now);
    let run = run_of(&c, &nonce(&c)).unwrap();
    report(&c, &head(3), &nonce(&c), "failure");
    c.hub.race(Call::FlowStage, move |h| {
        let mut w = h.world.borrow_mut();
        let r = w
            .workflow_runs
            .get_mut(REPO)
            .unwrap()
            .iter_mut()
            .find(|r| r["id"] == run)
            .unwrap();
        r["conclusion"] = json!("cancelled");
    });
    let reports = c.pass(now + 1, false);
    assert!(
        reports[0]
            .error
            .as_ref()
            .is_some_and(|e| e.contains("scope changed")),
        "{reports:?}"
    );
    assert!(c.deferred.0.borrow().is_empty());
}

#[test]
fn cancellation_progress_covers_multiple_attempts_of_the_same_check() {
    let c = configured();
    c.clean_review();
    let now = jobs::now();
    c.hub.cap_run_list(Some(0));
    let interval = c.config.settings.deferred.start_timeout;
    for attempt in 0..3 {
        assert!(matches!(
            step(&c, now + attempt * interval),
            Step::StartDeferred(_)
        ));
    }
    assert_eq!(runs(&c), 3);
    c.hub.cap_run_list(None);
    c.hub.ignore_cancels(true);
    c.hub.fail(Call::CancelRun);
    let due = now + 2 * interval + c.config.settings.deferred.run_timeout + 1;
    for offset in 0..3 {
        assert!(c.pass(due + offset, false)[0].error.is_some());
    }
    let asked = c.hub.asked(Call::CancelRun);
    let ids: std::collections::BTreeSet<_> = asked.iter().map(|a| a[1].clone()).collect();
    assert_eq!(ids.len(), 3);
    let reports = c.pass(due + 3, false);
    assert_eq!(reports[0].next.as_ref().unwrap().wait_until, Some(due + 60));
    assert_eq!(c.record().dispatches.len(), 3);
}

#[test]
fn removing_a_gate_cannot_land_before_uncertain_owned_work_is_cleaned_up() {
    let mut c = configured();
    c.clean_review();
    let now = jobs::now();
    c.hub.cap_run_list(Some(0));
    step(&c, now);
    c.config.settings.deferred.checks.clear();
    assert!(matches!(step(&c, now + 1000), Step::Wait { .. }));
    assert_eq!(c.hub.count(Call::Land), 0);
    c.hub.cap_run_list(None);
    assert!(matches!(step(&c, now + 1060), Step::CancelDeferred(_)));
    assert_eq!(step(&c, now + 1061), Step::Merge);
}

#[test]
fn failures_without_a_reviewer_or_a_reported_job_still_reach_repair() {
    for reviewer in [false, true] {
        let mut c = configured();
        if reviewer {
            c.clean_review();
        } else {
            c.config.settings.reviewers.clear();
        }
        let now = jobs::now();
        step(&c, now);
        let run = run_of(&c, &nonce(&c)).unwrap();
        c.hub.finish_workflow(REPO, run, "failure");
        assert_eq!(step(&c, now + 1), Step::HandFix(Kind::Red));
        let invocation: jobs::Invocation =
            serde_json::from_slice(&std::fs::read(c.deferred.0.borrow().last().unwrap()).unwrap())
                .unwrap();
        let Task::Fix { problems, .. } = invocation.task else {
            panic!("expected fix")
        };
        assert!(
            problems
                .iter()
                .any(|p| matches!(p, Problem::RedCheck { text, .. }
            if text.contains(&format!("actions/runs/{run}"))))
        );
    }
}

#[test]
fn foreign_deferred_results_on_the_pr_head_cannot_masquerade_as_ordinary_ci() {
    for cheap_red in [false, true] {
        let c = configured();
        c.clean_review();
        let now = jobs::now();
        step(&c, now);
        report(&c, &head(3), "foreign", "failure");
        if cheap_red {
            c.hub.ci(&head(3), &[("check", "failure")]);
        }
        let next = step(&c, now + 1);
        if cheap_red {
            assert_eq!(next, Step::HandFix(Kind::Red));
        } else {
            assert!(matches!(next, Step::Wait { .. }));
        }
        assert_eq!(c.hub.count(Call::Land), 0);
    }
}

#[test]
fn deferred_startup_suites_recover_while_ordinary_startup_failures_still_reach_repair() {
    for deferred in [false, true] {
        let c = configured();
        c.clean_review();
        let now = jobs::now();
        step(&c, now);
        let run = run_of(&c, &nonce(&c)).unwrap();
        c.hub.finish_workflow(REPO, run, "startup_failure");
        c.hub.edit_flow(REPO, 3, |s| {
            s.suites.insert(
                55,
                Suite {
                    status: "completed".into(),
                    conclusion: Some("startup_failure".into()),
                    app: "github-actions".into(),
                    workflow: Some(Workflow {
                        id: "55".into(),
                        name: "workflow".into(),
                        path: Some(format!(
                            ".github/workflows/{}",
                            if deferred { "heavy.yml" } else { "cheap.yml" }
                        )),
                    }),
                },
            );
        });
        let reports = c.pass(now + 1, false);
        assert_eq!(reports[0].error, None);
        if deferred {
            assert!(matches!(
                reports[0].next.as_ref().unwrap().step,
                Step::Wait { .. }
            ));
            assert_eq!(
                reports[0].next.as_ref().unwrap().wait_until,
                Some(now + c.config.settings.deferred.start_timeout)
            );
        } else {
            assert_eq!(
                reports[0].next.as_ref().unwrap().step,
                Step::HandFix(Kind::Red)
            );
        }
    }
}

#[test]
fn ordinary_jobs_keep_their_suite_metadata_in_a_configured_deferred_workflow() {
    let c = configured();
    c.hub.add_check(
        &head(3),
        json!({"id":1234,"name":"lint","status":"completed",
        "conclusion":"success","check_suite":{"id":55}}),
    );
    c.hub.edit_flow(REPO, 3, |s| {
        s.suites.insert(
            55,
            Suite {
                status: "in_progress".into(),
                conclusion: None,
                app: "github-actions".into(),
                workflow: Some(Workflow {
                    id: "55".into(),
                    name: "workflow".into(),
                    path: Some(".github/workflows/heavy.yml".into()),
                }),
            },
        );
    });
    let f = read(
        &c.hub,
        &REPO.parse().unwrap(),
        3,
        &c.policy,
        c.config.settings.clone(),
        &c.config.trusted_authors,
    )
    .unwrap();
    assert!(
        f.checks
            .iter()
            .any(|c| c.name == "workflow (still running)" && c.state == Ci::Pending)
    );
}

#[test]
fn proof_read_errors_still_retry_when_the_dispatch_budget_is_exhausted() {
    let mut c = configured();
    c.config.settings.deferred.attempts = 1;
    c.clean_review();
    let now = jobs::now();
    step(&c, now);
    report(&c, &head(3), &nonce(&c), "success");
    c.hub.fail(Call::Artifacts);
    let late = now + c.config.settings.deferred.start_timeout + 1;
    assert!(c.pass(late, false)[0].error.is_some());
    assert_eq!(c.record().dispatches.len(), 1);
    c.hub.heal(Call::Artifacts);
    assert_eq!(step(&c, late + 60), Step::Merge);
    assert_eq!(c.hub.count(Call::Dispatch), 1);
}

#[test]
fn a_failed_attempt_can_be_repaired_while_another_attempt_is_stopping() {
    let c = configured();
    c.clean_review();
    let now = jobs::now();
    c.hub.cap_run_list(Some(0));
    step(&c, now);
    let first = nonce(&c);
    step(&c, now + c.config.settings.deferred.start_timeout);
    let second = nonce(&c);
    c.hub.cap_run_list(None);
    report(&c, &head(3), &first, "failure");
    c.hub.ignore_cancels(true);
    let due =
        now + c.config.settings.deferred.start_timeout + c.config.settings.deferred.run_timeout;
    let reports = c.pass(due, false);
    assert_eq!(reports[0].error, None);
    assert_eq!(
        reports[0].next.as_ref().unwrap().step,
        Step::CancelDeferred(second)
    );
    assert_eq!(reports[0].next.as_ref().unwrap().wait_until, Some(due + 1));
    assert_eq!(step(&c, due + 1), Step::HandFix(Kind::Red));
    assert_eq!(runs(&c), 2);
}
