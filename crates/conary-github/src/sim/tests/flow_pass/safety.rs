//! Safety regressions for the explicit pass executor.
use super::*;

#[test]
fn pass_rechecks_same_head_scope_after_publication() {
    for field in ["r2", "hold", "needs-peter", "draft", "body", "base"] {
        for call in [Call::FlowStage, Call::FlowStatus] {
            let c = Case::new();
            c.clean_review();
            c.hub.race(call, move |hub| {
                if field == "draft" {
                    hub.edit_flow(REPO, 3, |s| s.draft = true);
                } else {
                    hub.edit_pull(REPO, 3, |p| match field {
                        "body" => p["body"] = json!("changed description"),
                        "base" => p["base"]["ref"] = json!("elsewhere"),
                        label => p["labels"]
                            .as_array_mut()
                            .unwrap()
                            .push(json!({"name":label})),
                    });
                    hub.refresh_flow(REPO, 3);
                }
            });
            let reports = c.pass(flow::jobs::now(), false);
            assert!(reports[0].error.is_some(), "{field} at {call:?}");
            assert_eq!(c.hub.count(Call::Land), 0, "{field} at {call:?}");
            assert!(
                !c.hub
                    .statuses(REPO, &head(3))
                    .unwrap()
                    .iter()
                    .find(|s| s.context == SETTLED)
                    .is_some_and(|s| s.state == "success")
            );
        }
    }
}

#[test]
fn pass_revokes_permission_even_when_stage_fails_and_on_retry() {
    let c = Case::new();
    flow::Write::status(&c.hub, REPO, &head(3), true, "old permission").unwrap();
    c.hub.fail(Call::FlowStage);
    let now = flow::jobs::now();
    for offset in [0, 1, 2] {
        c.pass(now + offset, false);
        assert_eq!(
            c.hub
                .statuses(REPO, &head(3))
                .unwrap()
                .iter()
                .find(|s| s.context == SETTLED)
                .unwrap()
                .state,
            "pending"
        );
        assert_eq!(c.hub.count(Call::Land), 0);
    }
}

#[test]
fn pass_conflict_request_uses_the_observed_base_commit() {
    for target in ["main", "other", "missing"] {
        for moved in [false, true] {
            let c = Case::new();
            let expected = if moved {
                c.hub.change(&head(7), TRUNK, "base movement")
            } else {
                TRUNK.into()
            };
            if target != "missing" {
                c.hub.set_branch(REPO, target, &expected);
            }
            c.hub
                .edit_pull(REPO, 3, |p| p["base"]["ref"] = json!(target));
            c.hub.refresh_flow(REPO, 3);
            c.hub.edit_flow(REPO, 3, |s| s.merge_state = "DIRTY".into());
            let reports = c.pass(flow::jobs::now(), false);
            let record = c.record();
            if target == "missing" {
                assert!(record.jobs.is_empty());
                assert_eq!(reports[0].error, None);
                let next = reports[0].next.as_ref().unwrap();
                assert!(matches!(next.step, flow::Step::Wait { .. }));
                assert!(next.description.contains("base commit is unavailable"));
                continue;
            }
            assert_eq!(reports[0].error, None);
            let invocation: flow::jobs::Invocation =
                serde_json::from_slice(&std::fs::read(&record.jobs[0].path).unwrap()).unwrap();
            let conary_core::plugin::Task::Fix { problems, .. } = invocation.task else {
                panic!("expected conflict fix");
            };
            assert!(problems.iter().any(|problem| matches!(problem,
                conary_core::plugin::Problem::Conflict { base, .. } if base == &expected)));
        }
    }
}

#[test]
fn pass_proof_slots_include_workers_started_in_this_pass_and_targeted_kicks() {
    for dry in [false, true] {
        let mut c = Case::new();
        c.config.plugins.proof = Some(example());
        c.config.settings.proof_slots = Some(1);
        c.config
            .settings
            .proofs
            .insert("machine".into(), Recipe::Run(vec!["check".into()]));
        c.hub.pr(REPO, 5, &head(5));
        c.hub
            .edit_pull(REPO, 5, |p| p["user"]["login"] = json!("owner"));
        let snapshot = flow::Read::snapshot(&c.hub, REPO, 3).unwrap();
        c.hub.flow_snapshot(
            REPO,
            5,
            Snapshot {
                revision: Revision::from(&c.hub.pull(REPO, 5).unwrap()),
                ..snapshot
            },
        );
        for pr in [3, 5] {
            c.hub.flow_review(REPO, pr, "reviewer", Verdict::Clean, 0);
            c.hub
                .post_status(&head(pr), json!({"context":"machine", "state":"pending"}));
        }
        let now = flow::jobs::now();
        let reports = c.pass(now, dry);
        assert_eq!(reports[0].next.as_ref().unwrap().step, Step::Prove);
        assert_eq!(reports[1].next.as_ref().unwrap().stage, Stage::ProofSlot);
        let reports = Pass {
            config: &c.config,
            policy: &c.policy,
            root: c.dir.path(),
            launcher: &c.deferred,
            now: now + 1,
            dry_run: dry,
            ledger: None,
        }
        .run(&c.hub, &REPO.parse().unwrap(), Some(5), None)
        .unwrap();
        assert_eq!(reports[0].next.as_ref().unwrap().stage, Stage::ProofSlot);
    }
}

#[test]
fn pass_local_judgment_preserves_newer_and_conflicting_authenticated_issuances() {
    for (generation, words) in [
        (2, vec!["failed"]),
        (2, vec!["owner"]),
        (2, vec!["unknown"]),
        (1, vec!["land", "failed"]),
    ] {
        let mut c = Case::new();
        c.config.plugins.judge = Some(example());
        c.config.settings.judge_engines = ["fixture-author".into()].into();
        c.config.settings.judge_key = Some(SealKey::from_hex(&"a".repeat(64)).unwrap());
        c.config.settings.verdict_authors = ["owner".into()].into();
        c.clean_review();
        c.hub.commit_details(
            &head(3),
            Some("change: fixture\n\nEngine: fixture-author\n"),
            Some("owner@example.org"),
            Some("owner@example.org"),
        );
        let now = flow::jobs::now();
        assert_eq!(
            c.pass(now, false)[0].next.as_ref().unwrap().step,
            Step::Judge(vec![Trigger::AuthorEngine])
        );
        c.run_worker();
        let revision = Revision::from(&c.hub.pull(REPO, 3).unwrap());
        for word in &words {
            let body = c.config.settings.judge_key.as_ref().unwrap().decision(
                REPO,
                3,
                "autoland-judgment",
                word,
                &revision,
                generation,
            );
            c.hub.flow_comment(REPO, 3, "owner", &body);
        }
        c.pass(now + 1, false);
        assert_eq!(c.hub.count(Call::Land), 0, "{words:?}");
        assert_eq!(c.record().memory.judge_issuance[&revision].0, generation);
        if words.len() > 1 || words[0] == "unknown" {
            assert_eq!(c.record().memory.judge_issuance[&revision].1, None);
        }
    }
}

#[test]
fn pass_direct_and_delegated_r2_require_the_candidate_train_after_owner_release() {
    for delegated in [false, true] {
        let mut c = if delegated {
            delegated_case()
        } else {
            Case::new()
        };
        c.config.settings.judge_key = Some(SealKey::from_hex(&"a".repeat(64)).unwrap());
        c.config.settings.verdict_authors = ["owner".into()].into();
        c.clean_review();
        c.hub
            .edit_pull(REPO, 3, |p| p["labels"] = json!([{"name":"r2"}]));
        let revision = Revision::from(&c.hub.pull(REPO, 3).unwrap());
        let body = c.config.settings.judge_key.as_ref().unwrap().decision(
            REPO,
            3,
            "autoland-judgment",
            "land",
            &revision,
            1,
        );
        c.hub.flow_comment(REPO, 3, "owner", &body);
        let now = flow::jobs::now();
        assert_eq!(
            c.pass(now, false)[0].next.as_ref().unwrap().step,
            Step::ListOwner
        );
        c.hub
            .edit_pull(REPO, 3, |p| p["labels"] = json!([{"name":"needs-peter"}]));
        c.pass(now + 1, false);
        c.hub.edit_pull(REPO, 3, |p| p["labels"] = json!([]));
        c.approve();
        let reports = c.pass(now + 2, false);
        assert!(matches!(
            reports[0].next.as_ref().unwrap().step,
            Step::Wait { .. }
        ));
        assert_eq!(c.hub.count(Call::Land), 0);
        assert_eq!(
            c.hub
                .statuses(REPO, &head(3))
                .unwrap()
                .iter()
                .find(|s| s.context == SETTLED)
                .unwrap()
                .state,
            "pending"
        );
    }
}

#[test]
fn pass_negative_evidence_is_required_before_direct_or_train_permission() {
    for train in [false, true] {
        for fault in [
            "missing config",
            "unsigned",
            "passed on base",
            "author runner",
            "other revision",
        ] {
            let mut c = Case::new();
            c.config.settings.train = train;
            c.clean_review();
            match fault {
                "missing config" => c.config.evidence = None,
                "unsigned" => strip_negative(&c),
                "passed on base" => {
                    use crate::attest::testing;
                    // A green workflow is not proof that the base tests failed.
                    let id = c.negative(&head(3), "none");
                    c.hub.forget_artifacts(Some(id));
                    let statement = testing::negative_statement(TRUNK, &head(3), "pass");
                    let claims = testing::claims(REPO, &head(3), id, &statement);
                    c.hub
                        .attest(id, &testing::file(&statement, &testing::token(&claims)));
                }
                "author runner" => {
                    let id = c.negative(&head(3), "none");
                    c.hub.ran_on(id, "author-worker");
                }
                _ => {
                    use crate::attest::testing;
                    let id = c.negative(&head(3), "none");
                    c.hub.forget_artifacts(Some(id));
                    let statement = testing::negative_statement(TRUNK, &head(4), "fail");
                    let claims = testing::claims(REPO, &head(3), id, &statement);
                    c.hub
                        .attest(id, &testing::file(&statement, &testing::token(&claims)));
                }
            }
            let reports = c.pass(flow::jobs::now(), false);
            assert!(
                matches!(reports[0].next.as_ref().unwrap().step, Step::Wait { .. }),
                "{fault}"
            );
            if fault != "missing config" {
                assert!(
                    reports[0]
                        .next
                        .as_ref()
                        .unwrap()
                        .description
                        .contains("negative"),
                    "{fault}: {:?}",
                    reports[0]
                );
            }
            assert_eq!(c.hub.count(Call::Land), 0);
            assert_eq!(
                c.hub
                    .statuses(REPO, &head(3))
                    .unwrap()
                    .iter()
                    .find(|s| s.context == SETTLED)
                    .unwrap()
                    .state,
                "pending"
            );
            assert!(
                c.store
                    .borrow()
                    .ledger()
                    .train(&REPO.parse().unwrap())
                    .is_none_or(|train| train.joined().is_empty())
            );
        }
    }
}

fn delegated_case() -> Case {
    let mut c = Case::new();
    c.config.settings.renovate_email = Some("bot@renovateapp.com".into());
    c.config.settings.renovate_login = Some("renovate[bot]".into());
    c.hub.edit_pull(REPO, 3, |p| {
        p["head"]["ref"] = json!("renovate/foo");
        p["title"] = json!("Update foo from 1.4 to 1.5");
        p["body"] = json!("🚦 **Automerge**: Enabled.");
    });
    c.hub.refresh_flow(REPO, 3);
    c.hub.commit_details(
        &head(3),
        Some("deps: bump"),
        Some("bot@renovateapp.com"),
        Some("bot@renovateapp.com"),
    );
    c.clean_review();
    c
}

#[test]
fn pass_delegated_settlement_requires_negative_evidence() {
    let c = delegated_case();
    strip_negative(&c);
    let reports = c.pass(flow::jobs::now(), false);
    assert!(matches!(
        reports[0].next.as_ref().unwrap().step,
        Step::Wait { .. }
    ));
    assert_eq!(
        c.hub
            .statuses(REPO, &head(3))
            .unwrap()
            .iter()
            .find(|s| s.context == SETTLED)
            .unwrap()
            .state,
        "pending"
    );
}

#[test]
fn pass_revokes_permission_when_the_success_write_races_a_scope_change() {
    for delegated in [false, true] {
        let c = if delegated {
            delegated_case()
        } else {
            Case::new()
        };
        c.clean_review();
        // First write revokes; the second grants. Mutate at the grant itself.
        c.hub.race(Call::FlowStatus, |hub| {
            hub.race(Call::FlowStatus, |hub| {
                hub.edit_pull(REPO, 3, |p| p["labels"] = json!([{"name":"hold"}]));
            });
        });
        let reports = c.pass(flow::jobs::now(), false);
        assert!(
            reports[0]
                .error
                .as_deref()
                .unwrap()
                .contains("scope changed")
        );
        assert_eq!(c.hub.count(Call::Land), 0);
        assert_eq!(
            c.hub
                .statuses(REPO, &head(3))
                .unwrap()
                .iter()
                .find(|s| s.context == SETTLED)
                .unwrap()
                .state,
            "pending"
        );
    }
}

fn strip_negative(c: &Case) {
    for run in c
        .hub
        .checks_on(&head(3))
        .iter()
        .filter(|r| r.name == "negative")
    {
        c.hub.forget_artifacts(Some(run.id));
    }
}
