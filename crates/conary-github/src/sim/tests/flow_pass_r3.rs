//! Regression cases from the pass integration review.
use super::flow_pass_case::{Case, REPO, example};
use super::{TRUNK, head};
use crate::api::Source;
use crate::flow::*;
use crate::sim::Call;
use serde_json::json;

fn run(c: &Case, only: Option<u64>, dry: bool) -> Vec<Report> {
    let mut store = c.store.borrow_mut();
    Pass {
        config: &c.config,
        policy: &c.policy,
        root: c.dir.path(),
        launcher: &c.deferred,
        now: jobs::now() + 10_000,
        dry_run: dry,
        ledger: None,
    }
    .run(&c.hub, &REPO.parse().unwrap(), only, Some(&mut store))
    .unwrap()
}
fn another(c: &Case) {
    c.hub.pr(REPO, 5, &head(5));
    c.hub
        .edit_pull(REPO, 5, |p| p["user"]["login"] = json!("owner"));
    let mut snapshot = c.hub.snapshot(REPO, 3).unwrap();
    snapshot.revision = Revision::from(&c.hub.pull(REPO, 5).unwrap());
    c.hub.flow_snapshot(REPO, 5, snapshot);
    c.good_ci(&head(5));
}
fn state(c: &Case, pr: u64) -> String {
    c.hub
        .statuses(REPO, &head(pr))
        .unwrap()
        .iter()
        .find(|s| s.context == SETTLED)
        .unwrap()
        .state
        .clone()
}
fn suite(c: &Case, id: u64, target: &str) {
    c.hub.edit_flow(REPO, 3, |s| {
        s.suites.insert(
            id,
            Suite {
                status: "completed".into(),
                conclusion: None,
                app: "github-actions".into(),
                workflow: Some(Workflow {
                    id: "gate".into(),
                    name: "gate".into(),
                    path: Some(".github/workflows/flake.yml".into()),
                }),
            },
        );
    });
    check(
        c,
        id,
        &format!("check / lane into {target}"),
        "success",
        None,
    );
}
fn check(c: &Case, suite: u64, name: &str, conclusion: &str, receipt: Option<&[u8]>) {
    let id = c.hub.run_id();
    c.hub.add_check(&head(3),json!({"id":id,"name":name,"status":"completed","conclusion":conclusion,"check_suite":{"id":suite}}));
    if conclusion == "success" {
        c.attest_run(&head(3), id, receipt);
    }
}
#[test]
fn admission_verifies_only_effective_checks_and_current_receipts() {
    let c = Case::new();
    c.clean_review();
    c.judgment(1, Judgment::Pass, true);
    let old_body = c
        .hub
        .reviews(REPO, 3)
        .unwrap()
        .last()
        .unwrap()
        .body
        .clone()
        .unwrap();
    c.hub.flow_review(REPO, 3, "reviewer", Verdict::Clean, 4);
    let body = c
        .hub
        .reviews(REPO, 3)
        .unwrap()
        .last()
        .unwrap()
        .body
        .clone()
        .unwrap();
    c.judgment(2, Judgment::Pass, true);
    let revision = Revision::from(&c.hub.pull(REPO, 3).unwrap());
    c.hub.remove_checks(&head(3));
    c.negative(&head(3), "none");
    suite(&c, 1, "main");
    check(&c, 1, "check", "cancelled", None);
    c.hub.edit_flow(REPO, 3, |s| {
        let mut legacy = s.suites[&1].clone();
        legacy.workflow.as_mut().unwrap().path =
            Some(".github/workflows/retired-receipts.yml".into());
        legacy.workflow.as_mut().unwrap().id = "retired-receipts".into();
        s.suites.insert(10, legacy);
    });
    check(
        &c,
        10,
        "conary/review/reviewer",
        "success",
        Some(&review_receipt(&head(3), &old_body)),
    );
    check(
        &c,
        10,
        "conary/judgment",
        "success",
        Some(&judgment_receipt(&revision, 1, Some(Judgment::Pass))),
    );
    suite(&c, 2, "other");
    check(&c, 2, "irrelevant failure", "failure", None);
    suite(&c, 3, "main");
    check(&c, 3, "check", "success", None);
    check(&c, 3, "unused / skipped", "skipped", None);
    check(
        &c,
        3,
        "conary/review/reviewer",
        "success",
        Some(&review_receipt(&head(3), &body)),
    );
    check(
        &c,
        3,
        "conary/judgment",
        "success",
        Some(&judgment_receipt(&revision, 2, Some(Judgment::Pass))),
    );
    let report = run(&c, None, false);
    assert_eq!(report[0].error, None, "{:?}", report[0]);
    assert_eq!(
        report[0].next.as_ref().unwrap().step,
        Step::Merge,
        "{:?}",
        report[0]
    );
    assert!(c.hub.pull(REPO, 3).unwrap().is_merged());
}
#[test]
fn excluded_prs_keep_read_classification_and_can_act_on_exclusion() {
    for excluded in ["untrusted", "draft", "hold", "incomplete", "stacked"] {
        let c = Case::new();
        match excluded {
            "untrusted" => c
                .hub
                .edit_pull(REPO, 3, |p| p["user"]["login"] = json!("outsider")),
            "draft" => c.hub.edit_flow(REPO, 3, |s| s.draft = true),
            "hold" => c.hub.label(REPO, 3, &["hold"]),
            "incomplete" => c.hub.edit_flow(REPO, 3, |s| s.complete = false),
            _ => {
                another(&c);
                c.hub
                    .edit_pull(REPO, 5, |p| p["head"]["ref"] = json!("pr-5"));
                c.hub
                    .edit_pull(REPO, 3, |p| p["base"]["ref"] = json!("pr-5"));
                c.hub.refresh_flow(REPO, 3);
            }
        }
        let report = run(&c, Some(3), false);
        assert_eq!(report[0].error, None, "{excluded}: {:?}", report[0]);
        assert_eq!(c.hub.count(Call::Land), 0);
        if excluded == "untrusted" {
            assert_eq!(report[0].next.as_ref().unwrap().step, Step::ListOwner);
            assert!(
                c.hub.pull(REPO, 3).unwrap().labels.iter().any(|l| c
                    .config
                    .settings
                    .owner_label
                    .as_ref()
                    == Some(&l.name))
            );
        }
    }
}
#[test]
fn a_targeted_kick_preserves_an_excluded_live_permission() {
    for train in [false, true] {
        let mut c = Case::new();
        if train {
            c.config.settings.train = true;
        } else {
            c.as_bot();
        }
        c.clean_review();
        let first = run(&c, Some(3), false);
        assert_eq!(first[0].error, None, "{:?}", first[0]);
        assert_eq!(state(&c, 3), "success");
        assert!(c.record().live.contains(&head(3)));
        another(&c);
        run(&c, Some(5), false);
        assert_eq!(state(&c, 3), "success", "train={train}");
        if train {
            assert!(
                !c.store
                    .borrow()
                    .ledger()
                    .train(&REPO.parse().unwrap())
                    .unwrap()
                    .joined()
                    .is_empty()
            );
        }
    }
}
#[test]
fn a_targeted_kick_recovers_uncertain_permission_on_an_excluded_pr() {
    let c = Case::new();
    another(&c);
    Write::status(&c.hub, REPO, &head(3), true, "unconfirmed").unwrap();
    let data = Data::open(c.dir.path(), &REPO.parse().unwrap()).unwrap();
    let mut record = Record::default();
    record.cleanup.insert(head(3));
    data.save(3, &record).unwrap();
    drop(data);
    run(&c, Some(5), false);
    assert_eq!(state(&c, 3), "pending");
    assert!(c.record().cleanup.is_empty());
}
#[test]
fn unsent_and_failed_merge_intents_can_be_revalidated_and_retried() {
    for failed in [false, true] {
        let c = Case::new();
        c.clean_review();
        if failed {
            c.hub.fail(Call::Land);
            run(&c, Some(3), false);
            c.hub.heal(Call::Land);
        } else {
            let data = Data::open(c.dir.path(), &REPO.parse().unwrap()).unwrap();
            let record = Record {
                landing: Some(crate::flow::Landing {
                    permission: None,
                    head: head(3),
                    target: "main".into(),
                    revision: None,
                    base: TRUNK.into(),
                    delegated: false,
                    accepted: false,
                    retry: false,
                }),
                ..Record::default()
            };
            data.save(3, &record).unwrap();
        }
        assert!(c.record().landing.is_some());
        for n in 0..4 {
            let mut store = c.store.borrow_mut();
            Pass {
                config: &c.config,
                policy: &c.policy,
                root: c.dir.path(),
                launcher: &c.deferred,
                now: jobs::now() + 20_000 + n * 20_000,
                dry_run: false,
                ledger: None,
            }
            .run(&c.hub, &REPO.parse().unwrap(), Some(3), Some(&mut store))
            .unwrap();
            if c.hub.pull(REPO, 3).unwrap().is_merged() {
                break;
            }
        }
        assert!(c.hub.pull(REPO, 3).unwrap().is_merged(), "failed={failed}");
        assert!(
            c.store
                .borrow()
                .ledger()
                .by_pr(&REPO.parse().unwrap(), 3)
                .unwrap()
                .landing
                .is_some()
        );
    }
}
#[test]
fn superseding_an_uncertain_head_does_not_block_the_new_revision() {
    let c = Case::new();
    c.clean_review();
    c.hub.fail(Call::Land);
    run(&c, Some(3), false);
    c.hub.heal(Call::Land);
    c.hub.push(REPO, 3, &head(4));
    c.hub.refresh_flow(REPO, 3);
    c.good_ci(&head(4));
    let reports = run(&c, Some(3), false);
    assert!(
        !reports[0]
            .next
            .as_ref()
            .unwrap()
            .description
            .contains("reconcile the submitted merge")
    );
    assert!(c.record().landing.as_ref().unwrap().retry);
}
#[test]
fn dry_run_with_a_writable_store_cannot_append_ledger_operations() {
    let c = Case::new();
    c.clean_review();
    let path = conary_core::log::log_path(c.store.borrow().lock().dir());
    let before = std::fs::read(&path).unwrap_or_default();
    let anchor = c.store.borrow().head();
    let reports = run(&c, None, true);
    assert_eq!(reports[0].next.as_ref().unwrap().step, Step::Merge);
    assert_eq!(std::fs::read(&path).unwrap_or_default(), before);
    assert_eq!(c.store.borrow().head(), anchor);
    assert_eq!(std::fs::read_dir(c.dir.path()).unwrap().count(), 0);
    for call in [
        Call::Land,
        Call::FlowStatus,
        Call::FlowStage,
        Call::FlowReview,
    ] {
        assert_eq!(c.hub.count(call), 0);
    }
}
#[test]
fn corrupt_proof_answers_are_one_unknown_holder_and_do_not_break_other_prs() {
    let mut c = Case::new();
    c.config.plugins.proof = Some(example());
    c.config.settings.proof_slots = Some(1);
    c.config
        .settings
        .proofs
        .insert("machine".into(), Recipe::Run(vec!["check".into()]));
    c.clean_review();
    c.hub
        .post_status(&head(3), json!({"context":"machine","state":"pending"}));
    run(&c, Some(3), false);
    let record = c.record();
    let work = &record.jobs[0];
    std::fs::write(work.path.with_extension("answer"), b"corrupt completion").unwrap();
    another(&c);
    for dry in [true, false] {
        let reports = run(&c, Some(5), dry);
        assert_eq!(reports[0].error, None);
        assert_eq!(
            reports[0].next.as_ref().unwrap().step,
            Step::QueueReview("reviewer".into())
        );
    }
    c.hub.flow_review(REPO, 5, "reviewer", Verdict::Clean, 0);
    c.hub
        .post_status(&head(5), json!({"context":"machine","state":"pending"}));
    for slots in [1, 2] {
        c.config.settings.proof_slots = Some(slots);
        for only in [None, Some(5)] {
            let reports = run(&c, only, true);
            let report = reports.iter().find(|r| r.pr == 5).unwrap();
            assert_eq!(report.error, None);
            assert_eq!(report.next.as_ref().unwrap().stage, Stage::ProofSlot);
        }
    }
}

#[test]
fn excluded_prs_can_cancel_obsolete_work_without_a_scope_mismatch() {
    let mut c = Case::new();
    c.config.plugins.proof = Some(example());
    c.config
        .settings
        .proofs
        .insert("machine".into(), Recipe::Run(vec!["check".into()]));
    c.clean_review();
    c.hub
        .post_status(&head(3), json!({"context":"machine","state":"pending"}));
    run(&c, Some(3), false);
    let path = c.record().jobs[0].path.clone();
    c.hub.push(REPO, 3, &head(4));
    c.hub.refresh_flow(REPO, 3);
    c.hub.edit_flow(REPO, 3, |s| s.draft = true);
    let report = run(&c, Some(3), false);
    assert_eq!(report[0].error, None);
    assert!(matches!(
        report[0].next.as_ref().unwrap().step,
        Step::StopJob(_)
    ));
    assert!(path.with_extension("cancel").exists());
}
