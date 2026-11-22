//! Durable admission, revocation and actual landing history.
use super::flow_pass_case::{Case, REPO};
use super::{TRUNK, head};
use crate::api::Source;
use crate::flow::*;
use crate::sim::Call;
use crate::train::Mark;

fn pending(c: &Case) {
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
#[test]
fn landing_is_recorded_once_with_the_actual_trunk_commits() {
    let c = Case::new();
    c.clean_review();
    let reports = c.pass(jobs::now(), false);
    assert_eq!(reports[0].error, None);
    let after = c.hub.pull(REPO, 3).unwrap().merge_commit_sha.unwrap();
    {
        let store = c.store.borrow();
        let change = store.ledger().by_pr(&REPO.parse().unwrap(), 3).unwrap();
        let landed = change
            .landing
            .as_ref()
            .expect("direct merge must be in the ledger");
        assert_eq!(landed.before.as_str(), TRUNK);
        assert_eq!(landed.after.as_str(), after);
        assert_eq!(
            change.revision(landed.revision).unwrap().head.as_str(),
            head(3)
        );
    }
    assert!(c.pass(jobs::now() + 1, false).is_empty());
    assert_eq!(c.hub.count(Call::Land), 1);
    // It lands as the train would: the PR's title, and its commits'
    // trailers once each, as the sim's default commit carries one.
    let title = c.hub.pull(REPO, 3).unwrap().title;
    let sent = &c.hub.asked(Call::Land)[0];
    assert_eq!(sent[3], format!("{title} (#3)"));
    assert_eq!(
        sent[4],
        "Co-authored-by: fieldmouseworks-workers[bot] <349565860+fieldmouseworks-workers[bot]@users.noreply.github.com>"
    );
}
#[test]
fn a_lost_merge_response_is_reconciled_after_the_pr_leaves_open() {
    let c = Case::new();
    c.clean_review();
    c.hub.lose(Call::Land);
    assert!(c.pass(jobs::now(), false)[0].error.is_some());
    assert!(c.record().landing.is_some());
    assert!(c.hub.pull(REPO, 3).unwrap().is_merged());
    c.hub.heal(Call::Land);
    c.pass(jobs::now() + 1, false);
    assert!(c.record().landing.is_none());
    assert!(
        c.store
            .borrow()
            .ledger()
            .by_pr(&REPO.parse().unwrap(), 3)
            .unwrap()
            .landing
            .is_some()
    );
    assert_eq!(c.hub.count(Call::Land), 1);
}
#[test]
fn delayed_merge_visibility_is_waited_for_without_a_second_merge() {
    let c = Case::new();
    c.clean_review();
    c.hub.race(Call::Land, |hub| {
        hub.stale(Call::Pull);
        hub.stale(Call::Pulls);
    });
    c.pass(jobs::now(), false);
    assert!(c.record().landing.is_some());
    let reports = c.pass(jobs::now() + 1, false);
    assert!(matches!(
        reports[0].next.as_ref().unwrap().step,
        Step::Wait { .. }
    ));
    assert_eq!(c.hub.count(Call::Land), 1);
    c.hub.fresh(Call::Pull);
    c.hub.fresh(Call::Pulls);
    c.pass(jobs::now() + 2, false);
    assert!(c.record().landing.is_none());
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
#[test]
fn a_settled_renovate_pr_without_a_train_is_merged_by_the_flow() {
    // Renovate merges nothing in a repository conaryd lands (burrow#1318),
    // so a settled minor update with no train lands through the flow's own
    // expected-head merge, never waits on Renovate's automerge.
    let c = Case::renovate();
    assert!(!c.config.settings.train);
    assert_eq!(
        c.pass(jobs::now(), false)[0].next.as_ref().unwrap().step,
        Step::Merge
    );
    assert_eq!(c.hub.count(Call::Land), 1);
    assert!(c.hub.pull(REPO, 3).unwrap().is_merged());
    c.pass(jobs::now() + 1, false);
    assert!(c.record().landing.is_none());
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
#[test]
fn memory_raises_are_appended_and_imported_by_a_fresh_flow_record() {
    let c = Case::new();
    c.clean_review();
    let repo = REPO.parse().unwrap();
    let data = Data::open(c.dir.path(), &repo).unwrap();
    let mut record = Record::default();
    record.memory.raised.insert("r2".into());
    data.save(3, &record).unwrap();
    drop(data);
    c.pass(jobs::now(), false);
    assert!(
        c.store
            .borrow()
            .ledger()
            .by_pr(&repo, 3)
            .unwrap()
            .raised()
            .contains(&"r2".into())
    );
    let data = Data::open(c.dir.path(), &repo).unwrap();
    data.save(3, &Record::default()).unwrap();
    drop(data);
    let report = c.pass(jobs::now() + 1, false);
    assert!(!matches!(
        report[0].next.as_ref().unwrap().step,
        Step::Merge
    ));
    assert_eq!(
        c.store
            .borrow()
            .ledger()
            .by_pr(&repo, 3)
            .unwrap()
            .classes
            .last()
            .unwrap()
            .class,
        conary_core::Class::R2
    );
    assert!(c.record().memory.raised.contains("r2"));
    pending(&c);
    c.hub.push(REPO, 3, &head(4));
    c.hub.refresh_flow(REPO, 3);
    c.pass(jobs::now() + 2, false);
    assert!(
        c.store
            .borrow()
            .ledger()
            .by_pr(&repo, 3)
            .unwrap()
            .classes
            .last()
            .unwrap()
            .raised
            .contains(&"r2".into())
    );
}
#[test]
fn blocking_producer_results_racing_publication_retract_permission() {
    for blocking in ["findings", "failed", "owner", "tombstone", "conflict"] {
        let c = Case::new();
        c.clean_review();
        c.judgment(1, Judgment::Pass, true);
        let key = c.config.settings.judge_key.clone().unwrap();
        c.hub.race(Call::FlowStage, move |hub| {
            if blocking == "findings" {
                hub.flow_review(REPO, 3, Verdict::Findings, 1);
            } else {
                let revision = Revision::from(&hub.pull(REPO, 3).unwrap());
                let word = match blocking {
                    "tombstone" => "unknown",
                    "conflict" => "land",
                    other => other,
                };
                let body = key.decision(REPO, 3, "autoland-judgment", word, &revision, 2);
                hub.flow_comment(REPO, 3, "producer", &body);
                if blocking == "conflict" {
                    let body = key.decision(REPO, 3, "autoland-judgment", "failed", &revision, 2);
                    hub.flow_comment(REPO, 3, "producer", &body);
                }
            }
        });
        assert!(c.pass(jobs::now(), false)[0].error.is_some(), "{blocking}");
        pending(&c);
    }
}
#[test]
fn ambiguous_success_writes_are_revoked_immediately_and_recovery_survives_restart() {
    for cleanup_fails in [false, true] {
        let c = Case::new();
        c.clean_review();
        c.hub
            .lose(crate::sim::On::from(Call::FlowStatus).mark(Mark::Landing));
        c.hub.race(
            crate::sim::On::from(Call::FlowStatus).mark(Mark::Landing),
            move |hub| {
                hub.label(REPO, 3, &["hold"]);
                if cleanup_fails {
                    hub.fail(crate::sim::On::from(Call::FlowStatus).mark(Mark::Waiting));
                }
            },
        );
        assert!(c.pass(jobs::now(), false)[0].error.is_some());
        assert_eq!(c.hub.count(Call::Land), 0);
        if cleanup_fails {
            assert!(c.record().cleanup.contains(&head(3)));
        } else {
            pending(&c);
        }
        c.hub.heal(Call::FlowStatus);
        c.pass(jobs::now() + 1, false);
        assert!(c.record().cleanup.is_empty());
        pending(&c);
    }
}

#[test]
fn a_nondefault_branch_merge_does_not_manufacture_a_trunk_landing() {
    let c = Case::new();
    c.hub.set_branch(REPO, "other", TRUNK);
    c.hub
        .edit_pull(REPO, 3, |p| p["base"]["ref"] = serde_json::json!("other"));
    c.hub.refresh_flow(REPO, 3);
    c.clean_review();
    let reports = c.pass(jobs::now(), false);
    assert_eq!(reports[0].error, None);
    assert!(c.hub.pull(REPO, 3).unwrap().is_merged());
    let store = c.store.borrow();
    let change = store.ledger().by_pr(&REPO.parse().unwrap(), 3).unwrap();
    assert!(change.landing.is_none());
    assert_eq!(change.status(), conary_core::ledger::Status::Closed);
    assert_eq!(
        crate::branches::Branches::branch(&c.hub, REPO, "main")
            .unwrap()
            .as_deref(),
        Some(TRUNK)
    );
}

#[test]
fn a_later_read_failure_still_revokes_the_previous_seated_permission() {
    // A seat in the train is the permission that outlasts its pass.
    let mut c = Case::new();
    c.config.settings.train = true;
    c.clean_review();
    c.pass(jobs::now(), false);
    assert!(c.record().live.contains(&head(3)));
    c.hub.fail(Call::Pull);
    let report = c.pass(jobs::now() + 1, false);
    assert!(report[0].error.is_some());
    assert!(c.record().cleanup.is_empty());
    assert!(c.record().live.is_empty());
    pending(&c);
}

#[test]
fn read_only_ledger_access_cannot_submit_a_merge() {
    let c = Case::new();
    c.clean_review();
    let store = c.store.borrow();
    let report = Pass {
        config: &c.config,
        policy: &c.policy,
        root: c.dir.path(),
        launcher: &c.deferred,
        now: jobs::now(),
        dry_run: false,
        ledger: Some(store.ledger()),
    }
    .run(&c.hub, &REPO.parse().unwrap(), None, None)
    .unwrap();
    assert!(matches!(
        report[0].next.as_ref().unwrap().step,
        Step::Wait { .. }
    ));
    pending(&c);
}
/// Just before the `k`th comparison from now, `then` changes GitHub.
fn before_comparison(hub: &crate::sim::Hub, k: usize, then: fn(&crate::sim::Hub)) {
    if k == 1 {
        hub.race(Call::CompareCommits, then);
    } else {
        hub.race(Call::CompareCommits, move |h| {
            before_comparison(h, k - 1, then)
        });
    }
}

#[test]
fn a_merge_whose_trailers_cant_all_be_read_isnt_made() {
    // The merge's own read of the PR's commits is made before it takes the
    // ledger's lock: only its last look's two reads of the scope come after.
    let clean = Case::new();
    clean.clean_review();
    clean.pass(jobs::now(), false);
    let own = clean.hub.count(Call::CompareCommits) - 2;
    assert_eq!(clean.hub.count(Call::Land), 1);
    let cut: fn(&crate::sim::Hub) = |h| h.short_history(TRUNK, &head(3), 0);
    let bare: fn(&crate::sim::Hub) = |h| h.commit_details(&head(3), None, None, None);
    for (then, why) in [
        (
            cut,
            "GitHub listed only 0 of its commits, so its landing can't say all their trailers",
        ),
        (
            bare,
            "GitHub sent its commit 0000000000000000000000000000000000000003 without a message",
        ),
    ] {
        let c = Case::new();
        c.clean_review();
        before_comparison(&c.hub, own, then);
        let reports = c.pass(jobs::now(), false);
        assert_eq!(c.hub.count(Call::CompareCommits), own);
        assert_eq!(reports[0].error.as_deref(), Some(why));
        assert_eq!(c.hub.count(Call::Land), 0);
        assert!(c.record().landing.is_none());
    }
}

/// A repo whose gate (`fresh / check`) runs only in its train's candidates,
/// never on a PR's head, as remi's does; `train` says whether its PRs land
/// through the train.
fn gate_on_candidates_only(train: bool) -> Case {
    let mut c = Case::new();
    c.clean_review();
    c.config.settings.train = train;
    c.config.evidence.as_mut().unwrap().require = vec!["fresh / check".into()];
    c
}

fn next(report: &[Report]) -> Next {
    assert_eq!(report[0].error, None);
    report[0].next.clone().unwrap()
}

#[test]
fn a_train_pr_joins_and_waits_in_the_train_while_its_gate_runs_only_on_the_candidate() {
    let c = gate_on_candidates_only(true);
    let now = jobs::now();
    let dry = next(&c.pass(now, true));
    assert_eq!(dry.step, Step::JoinTrain, "{}", dry.description);
    assert_eq!(next(&c.pass(now, false)).step, Step::JoinTrain);
    let seated = next(&c.pass(now + 1, false));
    assert_eq!(
        (seated.stage, seated.step),
        (Stage::Train, Step::Wait { until: None }),
        "{}",
        seated.description
    );
    assert!(seated.description.starts_with("In the train"));
}

#[test]
fn a_train_pr_still_needs_its_head_s_attested_negative_to_join() {
    let c = gate_on_candidates_only(true);
    c.hub.remove_check(&head(3), "negative");
    let n = next(&c.pass(jobs::now(), true));
    assert_eq!(
        (n.stage, n.step),
        (Stage::Proof, Step::Wait { until: None })
    );
    assert!(n.description.starts_with("negative"), "{}", n.description);
}

#[test]
fn a_pr_that_lands_itself_needs_the_gate_attested_on_its_head() {
    // Landing directly: the gate is asked of the head that lands.
    let c = gate_on_candidates_only(false);
    let n = next(&c.pass(jobs::now(), true));
    assert_eq!(
        (n.stage, n.step),
        (Stage::Proof, Step::Wait { until: None })
    );
    assert_eq!(
        n.description,
        "fresh / check: no attested check run (statuses cannot attest)"
    );
    // And a train repo's PR is asked it too, for any grant but the train's.
    let c = gate_on_candidates_only(true);
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
    let refused = crate::flow::evidence::authorize(
        &pass,
        &c.hub,
        &facts,
        &Step::Merge,
        &mut Default::default(),
    )
    .unwrap_err()
    .to_string();
    assert!(
        refused.starts_with("fresh / check: no attested check run"),
        "{refused}"
    );
}
