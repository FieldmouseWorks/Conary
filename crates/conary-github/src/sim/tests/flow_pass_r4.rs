//! Independent authorization and recovery must survive racing facts.
use super::flow_pass_case::{Case, REPO};
use super::{TRUNK, head};
use crate::api::Source;
use crate::flow::*;
use crate::sim::{Call, On};
use crate::train::Mark;
use serde_json::json;
fn permission(c: &Case) -> String {
    c.hub
        .statuses(REPO, &c.hub.pull(REPO, 3).unwrap().head.sha)
        .unwrap()
        .iter()
        .find(|s| s.context == SETTLED)
        .unwrap()
        .state
        .clone()
}
fn owner_case(r2: bool) -> Case {
    let mut c = Case::new();
    c.clean_review();
    if r2 {
        c.config.settings.train = true;
        c.hub.label(REPO, 3, &["r2"]);
    }
    c.judgment(1, if r2 { Judgment::Pass } else { Judgment::Owner }, true);
    c
}
#[test]
fn label_removal_and_unscoped_or_automated_reviews_cannot_approve_owner_decisions() {
    for r2 in [false, true] {
        for fault in [
            "labels",
            "author",
            "bot",
            "head",
            "intent",
            "target",
            "dismissed",
        ] {
            let c = owner_case(r2);
            let now = jobs::now();
            c.pass(now, false);
            c.pass(now + 1, false);
            c.hub.edit_pull(REPO, 3, |p| p["labels"] = json!([]));
            let mut revision = Revision::from(&c.hub.pull(REPO, 3).unwrap());
            if fault == "intent" {
                revision.description = "another intent".into();
            }
            if fault == "target" {
                revision.base = "release".into();
            }
            let body = owner_approval(&REPO.parse().unwrap(), 3, &revision);
            if fault != "labels" {
                c.hub.person_review(
                    REPO,
                    3,
                    if fault == "author" {
                        "owner"
                    } else {
                        "maintainer"
                    },
                    if fault == "bot" { "Bot" } else { "User" },
                    &head(if fault == "head" { 4 } else { 3 }),
                    &body,
                    if fault == "dismissed" {
                        "DISMISSED"
                    } else {
                        "APPROVED"
                    },
                );
            }
            c.pass(now + 2, false);
            assert_eq!(c.hub.count(Call::Land), 0, "{r2} {fault}");
            assert_ne!(permission(&c), "success", "{r2} {fault}");
            let facts = read(
                &c.hub,
                &REPO.parse().unwrap(),
                3,
                &c.policy,
                c.config.settings.clone(),
                &c.config.trusted_authors,
            )
            .unwrap();
            assert!(!facts.owner_approved, "{r2} {fault}");
        }
    }
}
#[test]
fn authorized_scoped_person_approval_allows_owner_decisions_and_revocation_blocks_the_train() {
    for r2 in [false, true] {
        let c = owner_case(r2);
        let now = jobs::now();
        c.pass(now, false); // retain the controller's owner label
        c.approve();
        let reports = c.pass(now + 1, false);
        assert_eq!(reports[0].error, None, "{:?}", reports[0]);
        assert_eq!(
            reports[0].next.as_ref().unwrap().step,
            if r2 { Step::JoinTrain } else { Step::Merge }
        );
        if r2 {
            assert_eq!(permission(&c), "success");
            let revision = Revision::from(&c.hub.pull(REPO, 3).unwrap());
            c.hub.person_review(
                REPO,
                3,
                "maintainer",
                "User",
                &head(3),
                &owner_approval(&REPO.parse().unwrap(), 3, &revision),
                "DISMISSED",
            );
            c.pass(now + 2, false);
            assert_ne!(permission(&c), "success");
            assert!(
                c.store
                    .borrow()
                    .ledger()
                    .train(&REPO.parse().unwrap())
                    .unwrap()
                    .joined()
                    .is_empty()
            );
        } else {
            assert!(c.hub.pull(REPO, 3).unwrap().is_merged());
        }
    }
}
#[test]
fn owner_approval_is_rechecked_when_permission_is_published() {
    for joined in [false, true] {
        let c = owner_case(true);
        c.approve();
        let now = jobs::now();
        if joined {
            assert_eq!(c.pass(now, false)[0].error, None);
        }
        c.hub
            .race(On::from(Call::FlowStatus).mark(Mark::Landing), |h| {
                h.person_review(
                    REPO,
                    3,
                    "maintainer",
                    "User",
                    &head(3),
                    "approval withdrawn",
                    "DISMISSED",
                );
            });
        let report = c.pass(now + 1, false);
        assert!(report[0].error.is_some());
        assert_ne!(permission(&c), "success");
        assert_eq!(c.hub.count(Call::Land), 0);
        if !joined {
            assert!(
                c.store
                    .borrow()
                    .ledger()
                    .train(&REPO.parse().unwrap())
                    .is_none_or(|t| t.joined().is_empty())
            );
        }
    }
}

#[test]
fn final_authorization_rejects_checks_that_stop_succeeding() {
    for machine in [false, true] {
        for state in ["failure", "pending", "missing"] {
            let mut c = Case::new();
            c.clean_review();
            let name = if machine { "machine" } else { "extra" };
            c.sign_check(&head(3), name, None);
            if machine {
                c.config
                    .settings
                    .proofs
                    .insert(name.into(), Recipe::Run(vec!["check".into()]));
            }
            // Arrive inside authorize, after permission's preceding scope read.
            c.hub
                .race(On::from(Call::FlowStatus).mark(Mark::Landing), move |h| {
                    h.race(Call::Reviews, move |h| {
                        h.race(Call::Reviews, move |h| {
                            h.race(Call::CheckRuns, move |h| {
                                if state == "missing" {
                                    h.remove_check(&head(3), name);
                                } else {
                                    h.post_status(&head(3), json!({"context":name,"state":state}));
                                }
                            });
                        });
                    });
                });
            c.pass(jobs::now(), false);
            assert_eq!(c.hub.count(Call::Land), 0, "{name} {state}");
            assert_ne!(permission(&c), "success");
        }
    }
}
#[test]
fn transient_discovery_failures_never_create_cancellable_jobs() {
    for slots in [None, Some(1)] {
        let mut c = Case::new();
        c.config.settings.proof_slots = slots;
        c.hub.race(Call::Pull.pr(3), |h| {
            h.fail(Call::Pull.pr(3));
            h.race(Call::Pull.pr(3), |h| h.heal(Call::Pull));
        });
        let report = c.pass(jobs::now(), false);
        assert_eq!(report[0].error, None, "{:?}", report[0]);
        assert_eq!(
            report[0].next.as_ref().unwrap().step,
            Step::QueueReview("reviewer".into())
        );
        assert!(c.record().memory.stopping.is_empty());
        c.pass(jobs::now() + 1, false);
        assert!(c.record().memory.stopping.is_empty());
    }
}
#[test]
fn retargeted_uncertain_intents_use_original_scope_and_allow_revalidated_progress() {
    let c = Case::new();
    c.clean_review();
    let advanced = c.hub.change(&head(20), TRUNK, "new trunk");
    c.hub.set_branch(REPO, "main", &advanced);
    c.hub.fail(Call::Land);
    c.pass(jobs::now(), false);
    c.hub.heal(Call::Land);
    assert_eq!(c.record().landing.as_ref().unwrap().target, "main");
    c.hub.set_branch(REPO, "release", TRUNK);
    c.hub.push(REPO, 3, &head(4));
    c.hub.edit_pull(REPO, 3, |p| {
        p["base"]["ref"] = json!("release");
        p["base"]["sha"] = json!(TRUNK);
    });
    c.hub.refresh_flow(REPO, 3);
    c.good_ci(&head(4));
    let comparisons = c.hub.count(Call::CompareCommits);
    c.pass(jobs::now() + 20_000, false); // consume the old failure
    assert!(
        c.hub.asked(Call::CompareCommits)[comparisons..]
            .iter()
            .any(|args| args[1] == advanced && args[2] == advanced),
        "recovery must inspect main, not release"
    );
    let reports = c.pass(jobs::now() + 40_000, false);
    assert!(
        !reports[0]
            .next
            .as_ref()
            .unwrap()
            .description
            .contains("reconcile the submitted merge")
    );
    assert!(c.record().landing.as_ref().unwrap().retry);
    assert_eq!(c.record().landing.as_ref().unwrap().target, "main");
    c.hub.flow_review(REPO, 3, "reviewer", Verdict::Clean, 5);
    let body = c
        .hub
        .reviews(REPO, 3)
        .unwrap()
        .last()
        .unwrap()
        .body
        .clone()
        .unwrap();
    c.sign_check(
        &head(4),
        "conary/review/reviewer",
        Some(&review_receipt(&head(4), &body)),
    );
    let root = c.dir.path().to_path_buf();
    c.hub.race(Call::Land, move |_| {
        let record = Data::read(&root, &REPO.parse().unwrap(), 3).unwrap();
        assert!(
            record
                .pending
                .iter()
                .any(|p| p.target == "main" && p.head == head(3))
        );
    });
    let report = c.pass(jobs::now() + 60_000, false);
    assert_eq!(report[0].error, None, "{:?}", report[0]);
    assert!(c.hub.pull(REPO, 3).unwrap().is_merged());
    assert_eq!(
        crate::branches::Branches::branch(&c.hub, REPO, "main")
            .unwrap()
            .as_deref(),
        Some(advanced.as_str())
    );
    assert!(
        c.store
            .borrow()
            .ledger()
            .by_pr(&REPO.parse().unwrap(), 3)
            .unwrap()
            .landing
            .is_none()
    );
}

#[test]
fn uncertain_intents_recover_through_long_and_non_first_parent_histories() {
    use crate::branches::Branches;
    for mode in ["long", "second-parent", "cut-superseded"] {
        let c = Case::new();
        c.clean_review();
        let old = c.hub.change(&head(20), TRUNK, "original base");
        c.hub.set_branch(REPO, "main", &old);
        c.hub.fail(Call::Land);
        c.pass(jobs::now(), false);
        c.hub.heal(Call::Land);
        let mut tip = old.clone();
        if mode == "second-parent" {
            tip = c.hub.change(&head(21), TRUNK, "other parent");
            c.hub.set_branch(REPO, "main", &tip);
            assert!(matches!(
                c.hub.merge(REPO, "main", &old, "merge parents").unwrap(),
                crate::branches::Merged::Commit { .. }
            ));
        } else {
            for n in 1000..1302 {
                tip = c.hub.change(&head(n), &tip, &format!("patch-{n}"));
            }
            c.hub.set_branch(REPO, "main", &tip);
            if mode == "cut-superseded" {
                c.hub.short_history(&old, &tip, 1);
                c.hub.push(REPO, 3, &head(4));
                c.hub.refresh_flow(REPO, 3);
                c.good_ci(&head(4));
            }
        }
        c.pass(jobs::now() + 20_000, false);
        assert!(c.record().landing.as_ref().unwrap().retry, "{mode}");
    }
}

#[test]
fn persisted_capacity_cancellation_from_an_older_pass_is_recovered() {
    let c = Case::new();
    let now = jobs::now();
    let data = Data::open(c.dir.path(), &REPO.parse().unwrap()).unwrap();
    let mut record = Record::default();
    record.memory.head = head(3);
    record.memory.stopping.insert("unreadable-3-0".into());
    record.memory.retry = Some((Failure::Stop, now + 10_000));
    record.memory.strikes.insert(Failure::Stop, 3);
    record.failed = Some(Failure::Stop);
    data.save(3, &record).unwrap();
    drop(data);
    let report = c.pass(now, false);
    assert_eq!(report[0].error, None);
    assert_eq!(
        report[0].next.as_ref().unwrap().step,
        Step::QueueReview("reviewer".into())
    );
    assert!(c.record().memory.stopping.is_empty());
}

#[test]
fn fresh_authorization_observations_cannot_drop_blocking_or_missing_checks() {
    for machine in [false, true] {
        for state in ["failure", "pending", "missing"] {
            let mut c = Case::new();
            c.clean_review();
            let name = if machine { "machine" } else { "extra" };
            c.sign_check(&head(3), name, None);
            if machine {
                c.config
                    .settings
                    .proofs
                    .insert(name.into(), Recipe::Run(vec!["check".into()]));
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
            c.hub.race(Call::CheckRuns, move |h| {
                if state == "missing" {
                    h.remove_check(&head(3), name);
                } else {
                    h.post_status(&head(3), json!({"context":name,"state":state}));
                }
            });
            let pass = Pass {
                config: &c.config,
                policy: &c.policy,
                root: c.dir.path(),
                launcher: &c.deferred,
                now: jobs::now(),
                dry_run: true,
                ledger: None,
            };
            assert!(
                crate::flow::evidence::authorize(&pass, &c.hub, &facts).is_err(),
                "{machine} {state}"
            );
        }
    }
}

#[test]
fn configured_machine_proofs_cannot_be_replaced_by_neutral_or_skipped_runs() {
    for conclusion in ["neutral", "skipped"] {
        let mut c = Case::new();
        c.clean_review();
        c.config
            .settings
            .proofs
            .insert("machine".into(), Recipe::Run(vec!["check".into()]));
        let id = c.hub.run_id();
        c.hub.add_check(
            &head(3),
            json!({"id":id,"name":"machine","status":"completed","conclusion":conclusion}),
        );
        let facts = read(
            &c.hub,
            &REPO.parse().unwrap(),
            3,
            &c.policy,
            c.config.settings.clone(),
            &c.config.trusted_authors,
        )
        .unwrap();
        let pass = Pass {
            config: &c.config,
            policy: &c.policy,
            root: c.dir.path(),
            launcher: &c.deferred,
            now: jobs::now(),
            dry_run: true,
            ledger: None,
        };
        assert!(crate::flow::evidence::authorize(&pass, &c.hub, &facts).is_err());
    }
}
