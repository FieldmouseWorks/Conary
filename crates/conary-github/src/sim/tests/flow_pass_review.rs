//! The executor's admission boundary: assertions do not confer permission.
use super::flow_pass_case::{Case, REPO, example};
use super::{TRUNK, head};
use crate::api::Source;
use crate::flow::*;
use crate::sim::{Call, On};
use crate::train::Mark;
use serde_json::json;

fn pending(c: &Case) {
    let statuses = c.hub.statuses(REPO, &head(3)).unwrap();
    assert_eq!(
        statuses
            .iter()
            .find(|s| s.context == SETTLED)
            .unwrap()
            .state,
        "pending"
    );
    assert!(!c.hub.pull(REPO, 3).unwrap().is_merged());
}
#[test]
fn unsigned_checks_and_review_markers_cannot_grant_permission() {
    for missing in ["check", "review"] {
        let c = Case::new();
        c.clean_review();
        let name = if missing == "check" {
            "check"
        } else {
            "conary/review/reviewer"
        };
        for run in c.hub.checks_on(&head(3)).iter().filter(|r| r.name == name) {
            c.hub.forget_artifacts(Some(run.id));
        }
        let report = c.pass(crate::flow::jobs::now(), false);
        assert_eq!(
            report[0].next.as_ref().unwrap().step,
            Step::Wait { until: None },
            "{missing}"
        );
        pending(&c);
        assert_eq!(c.hub.count(Call::Land), 0);
    }
}
#[test]
fn a_signature_from_the_authoring_runner_cannot_grant_permission() {
    let c = Case::new();
    c.clean_review();
    for run in c.hub.checks_on(&head(3)) {
        c.hub.ran_on(run.id, "authoring-session");
    }
    let report = c.pass(crate::flow::jobs::now(), false);
    assert!(
        report[0]
            .next
            .as_ref()
            .unwrap()
            .description
            .contains("fresh")
    );
    pending(&c);
}
#[test]
fn a_signed_review_for_another_result_cannot_grant_permission() {
    let c = Case::new();
    c.clean_review();
    // Same head and independently signed, but a different producer log.
    c.sign_check(
        &head(3),
        "conary/review/reviewer",
        Some(b"a different review"),
    );
    let report = c.pass(crate::flow::jobs::now(), false);
    assert!(
        report[0]
            .next
            .as_ref()
            .unwrap()
            .description
            .contains("exact result")
    );
    pending(&c);
}
#[test]
fn unsigned_machine_success_cannot_replace_a_running_proof() {
    let mut c = Case::new();
    c.clean_review();
    c.config
        .settings
        .proofs
        .insert("machine".into(), Recipe::Run(vec!["check".into()]));
    c.hub
        .post_status(&head(3), json!({"context":"machine","state":"success"}));
    let report = c.pass(crate::flow::jobs::now(), false);
    assert!(
        report[0]
            .next
            .as_ref()
            .unwrap()
            .description
            .contains("machine")
    );
    pending(&c);
}
#[test]
fn missing_or_passing_negative_never_authorizes_direct_or_train_permission() {
    for mode in ["direct", "train", "bot"] {
        for result in ["missing", "pass"] {
            let mut c = Case::new();
            c.clean_review();
            c.config.settings.train = mode == "train";
            if mode == "bot" {
                c.as_bot();
            }
            if result == "missing" {
                for run in c
                    .hub
                    .checks_on(&head(3))
                    .iter()
                    .filter(|r| r.name == "negative")
                {
                    c.hub.forget_artifacts(Some(run.id));
                }
            } else {
                c.negative(&head(3), "pass");
            }
            let dir = tempfile::tempdir().unwrap();
            let mut store = conary_core::Store::open(
                conary_core::log::Lock::take(dir.path()).unwrap(),
                ed25519_dalek::SigningKey::from_bytes(&[5; 32]),
                None,
            )
            .unwrap();
            let report = Pass {
                config: &c.config,
                policy: &c.policy,
                root: c.dir.path(),
                launcher: &c.deferred,
                now: crate::flow::jobs::now(),
                dry_run: false,
                ledger: None,
            }
            .run(&c.hub, &REPO.parse().unwrap(), None, Some(&mut store))
            .unwrap();
            assert_eq!(c.hub.count(Call::Land), 0, "{mode}/{result}");
            assert!(store.ledger().train(&REPO.parse().unwrap()).is_none());
            pending(&c);
            if result == "missing" {
                assert!(
                    report[0]
                        .next
                        .as_ref()
                        .unwrap()
                        .description
                        .contains("negative")
                );
            }
        }
    }
}
#[test]
fn a_train_admission_records_the_verified_negative() {
    let mut c = Case::new();
    c.clean_review();
    c.config.settings.train = true;
    let dir = tempfile::tempdir().unwrap();
    let mut store = conary_core::Store::open(
        conary_core::log::Lock::take(dir.path()).unwrap(),
        ed25519_dalek::SigningKey::from_bytes(&[5; 32]),
        None,
    )
    .unwrap();
    let report = Pass {
        config: &c.config,
        policy: &c.policy,
        root: c.dir.path(),
        launcher: &c.deferred,
        now: crate::flow::jobs::now(),
        dry_run: false,
        ledger: None,
    }
    .run(&c.hub, &REPO.parse().unwrap(), None, Some(&mut store))
    .unwrap();
    assert_eq!(report[0].error, None);
    let change = store.ledger().by_pr(&REPO.parse().unwrap(), 3).unwrap();
    assert!(
        change
            .evidence
            .iter()
            .any(|e| e.kind == conary_core::op::CheckKind::Negative && e.attest.is_some())
    );
}
fn needs_judge(c: &mut Case) {
    c.config.plugins.judge = Some(example());
    c.config.settings.judge_engines = ["fixture-author".into()].into();
    c.hub.commit_details(
        &head(3),
        Some("change: fixture\n\nEngine: fixture-author\n"),
        Some("owner@example.org"),
        Some("owner@example.org"),
    );
}
fn local_judgment(c: &Case) {
    for path in c.deferred.0.borrow_mut().drain(..) {
        crate::flow::jobs::worker(&path).unwrap();
    }
}
#[test]
fn a_local_judge_pass_is_not_an_authenticated_issuance() {
    let mut c = Case::new();
    c.clean_review();
    needs_judge(&mut c);
    let now = crate::flow::jobs::now();
    c.pass(now, false);
    local_judgment(&c);
    let report = c.pass(now + 1, false);
    assert!(matches!(
        report[0].next.as_ref().unwrap().step,
        Step::Wait { .. }
    ));
    assert!(c.record().memory.judge_issuance.is_empty());
    pending(&c);
}
#[test]
fn github_generations_and_tombstones_survive_local_judge_answers() {
    for word in ["fail", "owner", "conflict", "unsupported"] {
        let mut c = Case::new();
        c.clean_review();
        needs_judge(&mut c);
        let now = crate::flow::jobs::now();
        c.pass(now, false);
        local_judgment(&c);
        c.judgment(1, Judgment::Pass, true);
        // Observe generation 1 without letting the PR land.
        c.hub.fail(Call::Land);
        c.pass(now + 1, false);
        c.hub.heal(Call::Land);
        let revision = Revision::from(&c.hub.pull(REPO, 3).unwrap());
        if word == "conflict" {
            c.judgment(1, Judgment::Fail, true);
        } else {
            let key = c.config.settings.judge_key.as_ref().unwrap();
            let body = key.decision(REPO, 3, "autoland-judgment", word, &revision, 2);
            c.hub.flow_comment(REPO, 3, "producer", &body);
        }
        c.pass(now + 2, false);
        let record = c.record();
        let observed = record.memory.judge_issuance.get(&revision).unwrap();
        assert_eq!(observed.0, if word == "conflict" { 1 } else { 2 });
        assert_eq!(
            observed.1,
            match word {
                "fail" => Some(Judgment::Fail),
                "owner" => Some(Judgment::Owner),
                _ => None,
            }
        );
        assert!(!c.hub.pull(REPO, 3).unwrap().is_merged());
    }
}
#[test]
fn r2_owner_approval_does_not_enable_direct_or_delegated_landings() {
    for bot in [false, true] {
        let mut c = Case::new();
        c.clean_review();
        c.policy = conary_core::Policy::parse(br#"{"r2":["^src/"]}"#).unwrap();
        c.judgment(1, Judgment::Pass, true);
        // Person approval still cannot replace exact candidate evidence.
        c.approve();
        if bot {
            c.as_bot();
        }
        let facts = read(
            &c.hub,
            &REPO.parse().unwrap(),
            3,
            &c.policy,
            c.config.settings.clone(),
            &c.config.trusted_authors,
        )
        .unwrap();
        assert_eq!(
            next(&facts, &c.record().memory, crate::flow::jobs::now())
                .0
                .step,
            if bot { Step::Settle } else { Step::Merge }
        );
        let report = c.pass(crate::flow::jobs::now(), false);
        assert_eq!(
            report[0].next.as_ref().unwrap().step,
            Step::Wait { until: None }
        );
        assert_eq!(report[0].next.as_ref().unwrap().stage, Stage::Train);
        pending(&c);
    }
}
#[test]
fn same_head_scope_changes_are_checked_after_stage_publication() {
    for change in [
        "r2",
        "hold",
        "needs-peter",
        "draft",
        "target",
        "description",
    ] {
        let c = Case::new();
        c.clean_review();
        crate::branches::Branches::create_branch(&c.hub, REPO, "release", TRUNK).unwrap();
        c.hub.race(Call::FlowStage, move |h| match change {
            "draft" => h.edit_flow(REPO, 3, |s| s.draft = true),
            "target" => {
                h.retarget(REPO, 3, "release");
                h.refresh_flow(REPO, 3);
            }
            "description" => {
                h.edit_pull(REPO, 3, |p| p["body"] = json!("new claim"));
                h.refresh_flow(REPO, 3);
            }
            label => h.label(REPO, 3, &[label]),
        });
        let report = c.pass(crate::flow::jobs::now(), false);
        assert!(
            report[0].error.as_ref().unwrap().contains("scope"),
            "{change}"
        );
        assert_eq!(c.hub.count(Call::Land), 0);
        pending(&c);
    }
}
#[test]
fn scope_changes_during_permission_publication_are_revoked() {
    let c = Case::new();
    c.clean_review();
    c.hub
        .race(On::from(Call::FlowStatus).mark(Mark::Landing), |h| {
            h.edit_pull(REPO, 3, |p| p["body"] = json!("changed during success"));
            h.refresh_flow(REPO, 3);
        });
    let report = c.pass(crate::flow::jobs::now(), false);
    assert!(report[0].error.is_some());
    assert_eq!(c.hub.count(Call::Land), 0);
    pending(&c);
}
#[test]
fn final_scope_check_catches_a_change_after_permission_was_published() {
    let c = Case::new();
    c.clean_review();
    c.hub
        .race(On::from(Call::FlowStatus).mark(Mark::Landing), |h| {
            h.race(Call::Statuses, |h| {
                h.label(REPO, 3, &["hold"]);
            });
        });
    let report = c.pass(crate::flow::jobs::now(), false);
    assert!(report[0].error.is_some());
    assert_eq!(c.hub.count(Call::Land), 0);
    pending(&c);
}
#[test]
fn stage_failure_and_retry_cannot_leave_old_permission_live() {
    let c = Case::new();
    c.hub.fail(Call::FlowStage);
    let now = crate::flow::jobs::now();
    c.pass(now, false);
    pending(&c);
    c.hub.settle(&head(3));
    c.pass(now + 1, false);
    pending(&c);
}
#[test]
fn conflict_requests_pin_the_observed_base_commit() {
    let c = Case::new();
    c.clean_review();
    c.hub.edit_flow(REPO, 3, |s| s.merge_state = "DIRTY".into());
    let now = crate::flow::jobs::now();
    c.pass(now, false);
    c.pass(now + 1, false);
    let record = c.record();
    let work = record
        .jobs
        .iter()
        .find(|w| matches!(w.purpose, crate::flow::jobs::Purpose::Fix(Kind::Conflict)))
        .unwrap();
    let invocation: crate::flow::jobs::Invocation =
        serde_json::from_slice(&std::fs::read(&work.path).unwrap()).unwrap();
    assert!(
        matches!(invocation.task,conary_core::plugin::Task::Fix{problems,..}
        if problems.iter().any(|p|matches!(p,conary_core::plugin::Problem::Conflict{base,..} if base==TRUNK)))
    );
}

#[test]
fn a_train_departure_revokes_permission_even_if_its_label_write_fails() {
    let mut c = Case::new();
    c.clean_review();
    c.config.settings.train = true;
    let dir = tempfile::tempdir().unwrap();
    let mut store = conary_core::Store::open(
        conary_core::log::Lock::take(dir.path()).unwrap(),
        ed25519_dalek::SigningKey::from_bytes(&[5; 32]),
        None,
    )
    .unwrap();
    let now = crate::flow::jobs::now();
    let run = |store: &mut conary_core::Store, time| {
        Pass {
            config: &c.config,
            policy: &c.policy,
            root: c.dir.path(),
            launcher: &c.deferred,
            now: time,
            dry_run: false,
            ledger: None,
        }
        .run(&c.hub, &REPO.parse().unwrap(), None, Some(store))
        .unwrap()
    };
    assert_eq!(run(&mut store, now)[0].error, None);
    c.hub.edit_flow(REPO, 3, |s| s.open_threads = 1);
    c.hub.fail(Call::FlowStage);
    let report = run(&mut store, now + 1);
    assert_eq!(report[0].next.as_ref().unwrap().step, Step::LeaveTrain);
    assert!(report[0].error.is_some());
    pending(&c);
}
#[test]
fn the_runner_rejects_mutable_names_in_every_object_id_field() {
    use conary_core::plugin::{Identity, Limits, Plugin, Problem, Task};
    let plugin = Plugin::describe(
        &example(),
        Limits {
            deadline: std::time::Duration::from_secs(10),
            response_bytes: 1_048_576,
        },
    )
    .unwrap();
    let author = Identity {
        engine: "fixture-author".into(),
        session: None,
    };
    for task in [
        Task::Review {
            repo: REPO.into(),
            pr: 3,
            head: head(3),
            base: "main".into(),
            author: author.clone(),
        },
        Task::Judge {
            repo: REPO.into(),
            head: "main".into(),
            against: "criteria".into(),
            author,
        },
        Task::Proof {
            repo: REPO.into(),
            tree: "main".into(),
            checks: vec!["check".into()],
        },
        Task::Fix {
            repo: REPO.into(),
            head: head(3),
            problems: vec![Problem::Conflict {
                base: "main".into(),
                text: "conflict".into(),
            }],
        },
    ] {
        assert!(
            plugin
                .call(task)
                .unwrap_err()
                .to_string()
                .contains("object IDs")
        );
    }
}

#[test]
fn missing_attestation_waits_match_dry_and_mutating_passes() {
    let c = Case::new();
    c.clean_review();
    c.hub.forget_artifacts(None);
    let now = crate::flow::jobs::now();
    let dry = c.pass(now, true);
    assert_eq!(
        dry[0].next.as_ref().unwrap().step,
        Step::Wait { until: None }
    );
    assert_eq!(c.hub.count(Call::FlowStatus), 0);
    let real = c.pass(now, false);
    assert_eq!(dry[0].next, real[0].next);
    pending(&c);
}
#[test]
fn conflict_without_an_observed_base_commit_does_not_start_a_fixer() {
    let c = Case::new();
    c.clean_review();
    c.hub.edit_flow(REPO, 3, |s| s.merge_state = "DIRTY".into());
    crate::branches::Branches::delete_branch(&c.hub, REPO, "main").unwrap();
    let now = crate::flow::jobs::now();
    for at in [now, now + 1] {
        let report = c.pass(at, false);
        assert_eq!(report[0].error, None);
        let next = report[0].next.as_ref().unwrap();
        assert!(matches!(next.step, Step::Wait { .. }));
        assert!(
            next.description
                .contains("conflict base commit is unavailable")
        );
    }
    assert!(c.record().jobs.is_empty());
}

#[test]
fn a_native_request_for_changes_after_permission_is_published_blocks_the_merge() {
    let c = Case::new();
    c.clean_review();
    c.hub
        .race(On::from(Call::FlowStatus).mark(Mark::Landing), |h| {
            h.race(Call::Statuses, |h| {
                h.request_changes(REPO, 3, "human-reviewer")
            });
        });
    let report = c.pass(crate::flow::jobs::now(), false);
    assert!(
        report[0]
            .error
            .as_ref()
            .unwrap()
            .contains("changes were requested")
    );
    assert_eq!(c.hub.count(Call::Land), 0);
    pending(&c);
}
#[test]
fn a_signed_check_that_skipped_some_cannot_grant_permission() {
    let c = Case::new();
    c.clean_review();
    // The gate's job signs a pass that left a check unbuilt: the flow
    // reads no evidence for what it skipped.
    let tree = crate::branches::Branches::tree(&c.hub, REPO, &head(3)).unwrap();
    for run in c
        .hub
        .checks_on(&head(3))
        .iter()
        .filter(|r| r.name == "check")
    {
        c.hub.forget_artifacts(Some(run.id));
        let statement = crate::attest::testing::statement(&head(3), &tree, "pass")
            .replace(r#""skipped":[]"#, r#""skipped":["agents"]"#);
        let claims = crate::attest::testing::claims(REPO, &head(3), run.id, &statement);
        c.hub.attest(
            run.id,
            &crate::attest::testing::file(&statement, &crate::attest::testing::token(&claims)),
        );
    }
    let report = c.pass(crate::flow::jobs::now(), false);
    let next = report[0].next.as_ref().unwrap();
    assert!(
        next.description.contains("check: it skipped agents"),
        "{}",
        next.description
    );
    pending(&c);
}
