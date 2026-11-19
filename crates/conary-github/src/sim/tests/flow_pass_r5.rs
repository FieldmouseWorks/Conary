//! Submission ordering and faithful fixer handoffs.
use super::flow_pass_case::{Case, REPO};
use super::head;
use crate::api::Source;
use crate::flow::*;
use crate::sim::{Call, STEWARD};
use serde_json::json;

#[test]
fn cleanup_with_a_tied_merge_timestamp_preserves_submitted_authority() {
    for mode in [
        "lost",
        "delayed",
        "delayed-accepted",
        "post-failure",
        "restart-cleanup",
    ] {
        let c = Case::new();
        c.clean_review();
        if !matches!(mode, "post-failure" | "delayed-accepted") {
            c.hub.lose(Call::Land);
        } else if mode == "post-failure" {
            c.hub.race(Call::Land, |hub| hub.fail(Call::Commit));
        }
        if matches!(mode, "delayed" | "delayed-accepted") {
            c.hub.race(Call::Land, |hub| {
                hub.stale(Call::Pull);
                hub.stale(Call::Pulls);
            });
        }
        let report = c.pass(jobs::now(), false).remove(0);
        assert_eq!(report.error.is_some(), mode != "delayed-accepted");
        if mode == "delayed-accepted" {
            let report = c.pass(jobs::now() + 1, false).remove(0);
            assert_eq!(report.error, None);
            assert!(matches!(report.next.unwrap().step, Step::Wait { .. }));
        }
        let at = c
            .hub
            .statuses(REPO, &head(3))
            .unwrap()
            .into_iter()
            .find(|s| s.context == SETTLED && s.state == "success")
            .unwrap()
            .created_at
            .unwrap();
        if mode == "restart-cleanup" {
            let data = Data::open(c.dir.path(), &REPO.parse().unwrap()).unwrap();
            let mut record = data.load(3).unwrap();
            record.cleanup.insert(head(3));
            data.save(3, &record).unwrap();
        }
        let mut statuses = c.hub.statuses_json(&head(3));
        // GitHub's second resolution, unlike Hub's default per-event clock.
        statuses[0]["created_at"] = json!(at);
        c.hub.put_statuses(&head(3), statuses);
        c.hub.edit_pull(REPO, 3, |p| p["merged_at"] = json!(at));
        c.hub.heal(Call::Land);
        c.hub.heal(Call::Commit);
        c.hub.fresh(Call::Pull);
        c.hub.fresh(Call::Pulls);
        c.pass(jobs::now() + 1, false);
        assert!(
            c.store
                .borrow()
                .ledger()
                .by_pr(&REPO.parse().unwrap(), 3)
                .unwrap()
                .landing
                .is_some()
        );
        assert!(c.record().landing.is_none());
        assert_eq!(c.hub.count(Call::Land), 1);
    }
}

#[test]
fn cleanup_cannot_invent_authorization_or_override_a_real_withdrawal() {
    for invalid in [
        "missing",
        "actor",
        "scope",
        "base",
        "head",
        "target",
        "status",
        "withdrawn",
    ] {
        let c = Case::new();
        c.clean_review();
        c.hub.lose(Call::Land);
        c.pass(jobs::now(), false);
        let data = Data::open(c.dir.path(), &REPO.parse().unwrap()).unwrap();
        let mut record = data.load(3).unwrap();
        let landing = record.landing.as_mut().unwrap();
        let permission = &mut landing.permission;
        let at = permission.as_ref().unwrap().at.clone();
        match invalid {
            "missing" => *permission = None,
            "actor" => permission.as_mut().unwrap().login = "intruder".into(),
            "scope" => landing.revision.as_mut().unwrap().description = "other".into(),
            "base" => landing.base = head(99),
            "head" => landing.head = head(99),
            "target" => landing.target = "other".into(),
            "status" => {
                let mut statuses = c.hub.statuses_json(&head(3));
                for status in statuses.as_array_mut().unwrap() {
                    if status["context"] == SETTLED && status["state"] == "success" {
                        status["creator"]["login"] = json!("intruder");
                    }
                }
                c.hub.put_statuses(&head(3), statuses);
            }
            _ => c.hub.post_status(
                &head(3),
                json!({"context":SETTLED,"state":"pending",
                "description":"withdrawn before merge", "created_at":at,
                "creator":{"login":STEWARD,"type":"Bot"}}),
            ),
        }
        data.save(3, &record).unwrap();
        drop(data);
        c.hub.edit_pull(REPO, 3, |p| p["merged_at"] = json!(at));
        let mut statuses = c.hub.statuses_json(&head(3));
        statuses[0]["created_at"] = json!(at);
        c.hub.put_statuses(&head(3), statuses);
        c.hub.heal(Call::Land);
        c.pass(jobs::now() + 1, false);
        let store = c.store.borrow();
        let change = store.ledger().by_pr(&REPO.parse().unwrap(), 3).unwrap();
        assert!(change.landing.is_none(), "{invalid}");
        assert!(!store.ledger().outside().is_empty(), "{invalid}");
    }
}

#[test]
fn github_findings_supply_the_current_fixer_problems_without_local_workers() {
    use conary_core::plugin::Task;
    for mode in ["fresh", "carried"] {
        let c = Case::new();
        let now = jobs::now();
        // Two of the connector's findings, each on its thread: the earlier
        // one answered and resolved, the later one open.
        c.hub.flow_review(REPO, 3, Verdict::Findings, now + 2);
        for (id, text, resolved) in [
            ("PRRT_10", "superseded", true),
            ("PRRT_11", "current blocker", false),
        ] {
            c.hub.flow_thread_at(
                REPO,
                3,
                id,
                ("src/lib.rs", 7),
                (crate::sim::CONNECTOR, &format!("**P1** {text}")),
            );
            c.hub.flow_thread(REPO, 3, id, resolved);
        }
        if mode == "carried" {
            let tip = c.hub.add(super::TRUNK, "trunk-only patch");
            c.hub.set_branch(REPO, "main", &tip);
            let mut patches = c.hub.patches(&tip);
            patches.extend(c.hub.patches(&head(3)));
            c.hub.commit_as(&head(4), &[head(3), tip], patches);
            c.hub.push(REPO, 3, &head(4));
            c.hub.refresh_flow(REPO, 3);
            c.good_ci(&head(4));
        }
        c.pass(now + 4, false);
        let reports = c.pass(now + 5, false);
        assert_eq!(reports[0].error, None);
        assert_eq!(
            reports[0].next.as_ref().unwrap().step,
            Step::HandFix(Kind::Findings)
        );
        let path = c.deferred.0.borrow()[0].clone();
        let invocation: jobs::Invocation =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let Task::Fix { problems, .. } = invocation.task else {
            panic!("fix")
        };
        let wire = serde_json::to_string(&problems).unwrap();
        assert!(wire.contains("current blocker"));
        assert!(wire.contains("GitHub review thread PRRT_11"));
        assert!(!wire.contains("superseded"));
        assert_eq!(c.record().jobs[0].handed, [Some("PRRT_11".to_string())]);
    }
}

#[test]
fn unrepresentable_blockers_wait_instead_of_launching_an_empty_fixer() {
    for blocker in ["thread", "native", "judgment", "description"] {
        let c = Case::new();
        if matches!(blocker, "judgment" | "description") {
            c.clean_review();
            c.judgment(
                1,
                if blocker == "judgment" {
                    Judgment::Fail
                } else {
                    Judgment::Misdescribed
                },
                true,
            );
        } else {
            if blocker == "native" {
                c.hub.person_review(
                    REPO,
                    3,
                    "maintainer",
                    "User",
                    &head(3),
                    "Fix the observed bug",
                    "CHANGES_REQUESTED",
                );
            }
            if blocker == "thread" {
                // A person's thread on a whole file: no line to hand.
                c.hub.flow_thread_comments(
                    REPO,
                    3,
                    "file-thread",
                    &[("maintainer", "This file shouldn't exist")],
                );
                c.hub.flow_thread(REPO, 3, "file-thread", false);
            }
        }
        let now = jobs::now();
        c.pass(now, false);
        let report = c.pass(now + 1, false).remove(0);
        assert_eq!(report.error, None, "{blocker}");
        let next = report.next.unwrap();
        assert!(
            matches!(next.step, Step::Wait { .. }),
            "{blocker}: {next:?}"
        );
        assert!(next.description.contains("v1 fixer"), "{blocker}: {next:?}");
        assert!(c.deferred.0.borrow().is_empty(), "{blocker}");
        assert!(c.record().jobs.is_empty(), "{blocker}");
    }
}

#[test]
fn a_known_merge_refusal_does_not_leave_a_historical_permission_receipt() {
    let c = Case::new();
    c.clean_review();
    c.hub.refuse(REPO, 3, "not accepted");
    assert!(c.pass(jobs::now(), false)[0].error.is_some());
    let record = c.record();
    assert!(record.landing.unwrap().permission.is_none());
    assert!(!c.hub.pull(REPO, 3).unwrap().is_merged());
}
