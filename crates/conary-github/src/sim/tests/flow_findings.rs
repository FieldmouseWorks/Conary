//! Regressions for the eight review findings on #266 at 3dbc081.
use super::flow_case::Case;
use crate::api::Source;
use crate::flow::*;
use crate::sim::REMI;
use serde_json::json;

#[test]
fn untrusted_train_statuses_cannot_hide_the_latest_owned_result() {
    for context in ["conary/train", "CONARY/TRAIN"] {
        for who in [None, Some("outsider"), Some("fieldmouseworks-steward[bot]")] {
            for state in ["success", "pending", "failure", "error"] {
                let c = Case::new();
                let creator = who.map(|login| json!({"login":login,"type":"Bot"}));
                c.hub.put_statuses(&c.scope().head,json!([
                    {"context":context,"state":state,"creator":creator},
                    {"context":"conary/train","state":"failure","creator":{"login":"fieldmouseworks-steward[bot]","type":"Bot"}}
                ]));
                let owned = who == Some("fieldmouseworks-steward[bot]");
                let red = !owned || state == "failure";
                assert_eq!(
                    c.view(8000).step == Step::HandFix(Kind::Red),
                    red,
                    "{context} {who:?} {state}"
                );
            }
        }
    }
    let c = Case::new();
    c.hub.put_statuses(
        &c.scope().head,
        json!([{"context":"conary/train-security","state":"failure"}]),
    );
    assert_eq!(c.view(8000).step, Step::HandFix(Kind::Red));
}
#[test]
fn old_judge_and_sweep_tokens_cannot_be_reissued_by_copying_or_editing_comments() {
    for sweep in [false, true] {
        let mut c = Case::new();
        if sweep {
            c.sweep("ok");
        } else {
            c.judge("pass");
        }
        let old = c
            .hub
            .comments(REMI, 3)
            .unwrap()
            .last()
            .unwrap()
            .body
            .clone()
            .unwrap();
        if sweep {
            c.sweep("held");
        } else {
            c.judge("owner");
        }
        for replay in [&old, &format!("> {old}"), &format!("{old}\n{old}")] {
            c.hub.flow_comment(REMI, 3, "owner", replay);
            let f = c.facts();
            if sweep {
                assert_eq!(f.major, Some(Major::Held));
            } else {
                assert_eq!(f.judgment.unwrap().verdict, Judgment::Owner);
            }
        }
        // A genuinely new issuance on this same revision must remain possible.
        if sweep {
            c.sweep("ok");
            assert_eq!(c.facts().major, Some(Major::Cleared));
        } else {
            c.judge("pass");
            assert_eq!(c.facts().judgment.unwrap().verdict, Judgment::Pass);
        }
    }
}
#[test]
fn a_title_edit_invalidates_reviews_and_judgments_without_rebinding_legacy_scope() {
    for judged in [false, true] {
        let mut c = Case::new();
        if judged {
            c.judge("pass");
        }
        c.hub
            .edit_pull(REMI, 3, |p| p["title"] = json!("change: a different claim"));
        c.hub.refresh_flow(REMI, 3);
        // GitHub dates the edit after the head's push.
        c.hub.edit_flow(REMI, 3, |s| s.since.edited = Some(500));
        let f = c.facts();
        assert!(f.reviewers["reviewer"].changed);
        if judged {
            assert!(f.judgment.is_none());
        }
        // A judgment of the old title is one asked for again.
        let why = if judged {
            vec![Trigger::Asked, Trigger::Changed]
        } else {
            vec![Trigger::Changed]
        };
        assert_eq!(c.view(8000).step, Step::Judge(why));
    }
}
#[test]
fn dependency_landing_never_skips_existing_or_owed_reviewer_gates() {
    for major in [false, true] {
        for r2 in [false, true] {
            let mut c = Case::new();
            c.settings.train = true;
            c.settings.renovate_email = Some("bot@example.org".into());
            c.hub.edit_pull(REMI, 3, |p| {
                p["head"]["ref"] = json!("renovate/pkg");
                p["title"] = json!(if major {
                    "Update pkg from 1 to 2"
                } else {
                    "Update pkg from 1.0 to 1.1"
                });
            });
            c.hub.refresh_flow(REMI, 3);
            if r2 {
                c.secret();
            }
            c.sweep("ok");
            c.judge("pass");
            c.roles(&["reviewer"]);
            assert!(matches!(c.view(8000).step, Step::AskReview(_)));
            c.review("reviewer", Verdict::Findings, 600);
            c.threads(1);
            assert_eq!(c.view(8000).step, Step::HandFix(Kind::Findings));
        }
    }
}
#[test]
fn live_fix_ownership_blocks_landing_updates_and_competing_jobs_on_every_revision() {
    for kind in [
        Kind::Findings,
        Kind::Red,
        Kind::Conflict,
        Kind::Description,
        Kind::JudgeFailure,
    ] {
        for state in [JobState::Queued, JobState::Running, JobState::Unknown] {
            for scenario in 0..4 {
                let mut c = Case::new();
                let revision = c.scope();
                match scenario {
                    0 => c.settings.train = true,
                    1 => c
                        .hub
                        .edit_flow(REMI, 3, |s| s.merge_state = "BEHIND".into()),
                    2 => c.secret(),
                    _ => {
                        c.push("fix-owns-new-head");
                        c.review("reviewer", Verdict::Clean, 600);
                        c.hub.set_branch(REMI, "release", super::TRUNK);
                        c.hub.retarget(REMI, 3, "release");
                        c.hub.refresh_flow(REMI, 3);
                        c.review("reviewer", Verdict::Clean, 700);
                    }
                }
                c.jobs.insert(
                    kind,
                    Job {
                        id: "owner".into(),
                        revision,
                        state,
                        started: 700,
                        repeated: false,
                    },
                );
                let n = c.view(8000);
                assert_eq!(n.stage, Stage::Fix, "{kind:?} {state:?} {scenario}");
                assert!(matches!(n.step, Step::Wait { .. }));
            }
        }
    }
}
#[test]
fn only_the_actual_base_repository_and_branch_can_form_a_stack() {
    for repository in [
        None,
        Some("fork/repo"),
        Some(REMI),
        Some("fieldmouseworks/REMI"),
    ] {
        let c = Case::new();
        c.hub.pr(REMI, 7, &super::head(7));
        c.hub.edit_pull(REMI, 7, |p| {
            p["head"]["ref"] = json!("main");
            p["head"]["repo"] = repository
                .map(|name| json!({"full_name":name,"default_branch":"main"}))
                .unwrap_or(serde_json::Value::Null);
        });
        assert_eq!(
            c.facts().stacked_on,
            if repository.is_some_and(|r| r.eq_ignore_ascii_case(REMI)) {
                Some(7)
            } else {
                None
            }
        );
    }
}
#[test]
fn configured_proof_case_is_preserved_without_blocking_job_admission() {
    for context in [None, Some("vm"), Some("vM"), Some("VM")] {
        let mut c = Case::new();
        c.settings
            .proofs
            .insert("VM".into(), Recipe::Run(vec!["exercise".into()]));
        if let Some(context) = context {
            c.hub.put_statuses(
                &c.scope().head,
                json!([{"context":context,"state":"pending"}]),
            );
        }
        assert_eq!(c.view(8000).step, Step::Prepare);
        assert_eq!(
            c.view(8000).proof,
            Some(ProofPlan::Prepare(c.settings.proofs.clone()))
        );
    }
}

#[test]
fn observed_generations_survive_deleted_or_rewritten_decision_history() {
    for sweep in [false, true] {
        let mut c = Case::new();
        if sweep {
            c.hub.edit_pull(REMI, 3, |p| {
                p["head"]["ref"] = json!("renovate/pkg");
                p["title"] = json!("Update pkg from 1 to 2");
            });
            c.hub.refresh_flow(REMI, 3);
            c.review("reviewer", Verdict::Clean, 600);
            c.sweep("ok");
        } else {
            c.judge("pass");
        }
        // The connector's summary comes before it.
        let comments = c.hub.comments(REMI, 3).unwrap();
        let first = comments.len() - 1;
        let old = comments[first].body.clone().unwrap();
        if sweep {
            c.sweep("held");
        } else {
            c.judge("owner");
        }
        // A held major names no door: it's the sweep's wait, not a listing.
        let held = |n: &Next| {
            if sweep {
                n.step == Step::Wait { until: None }
                    && n.description.starts_with("Major update held")
            } else {
                n.step == Step::ListOwner(Door::Judge)
            }
        };
        let n = c.pass(8000);
        assert!(held(&n), "{n:?}");
        c.hub.edit_flow_comment(REMI, 3, first + 1, &old);
        let n = c.pass(8100);
        assert!(held(&n), "{n:?}"); // Keep the authenticated Owner/Held decision, not just its counter.
        assert!(!matches!(n.step, Step::Merge | Step::JoinTrain));
        if sweep {
            c.sweep("ok");
        } else {
            c.judge("pass");
        }
        assert_eq!(c.pass(8200).step, Step::Merge);
    }
}
#[test]
fn issuance_metadata_is_authenticated_and_conflicting_generations_fail_closed() {
    let mut c = Case::new();
    let key = SealKey::from_hex(&"a".repeat(64)).unwrap();
    c.settings.judge_key = Some(key.clone());
    c.settings.verdict_authors.insert("owner".into());
    // A producer's v3 pass; v2 issuances share its generations.
    let scope = c.scope();
    let basis = super::judged_for_everything(&c.hub, REMI);
    let pass = |generation| {
        key.judgment_by(
            REMI,
            3,
            Judgment::Pass,
            &scope,
            generation,
            super::judge_provenance(),
            basis.clone(),
        )
        .unwrap()
    };
    let land = pass(9);
    let tampered = land.replace("\"generation\":9", "\"generation\":99");
    let first = c.hub.comments(REMI, 3).unwrap().len();
    c.hub.flow_comment(REMI, 3, "owner", &tampered);
    assert!(c.facts().judgment.is_none());
    c.hub.flow_comment(REMI, 3, "owner", &land);
    let owner = key.decision(REMI, 3, "autoland-judgment", "owner", &c.scope(), 9);
    c.hub.flow_comment(REMI, 3, "owner", &owner);
    assert_eq!(c.pass(8000).step, Step::Judge(vec![Trigger::Asked]));
    c.hub.edit_flow_comment(REMI, 3, first + 2, "");
    assert_eq!(c.pass(8100).step, Step::Judge(vec![Trigger::Asked]));
    let newer = pass(10);
    c.hub.flow_comment(REMI, 3, "owner", &newer);
    assert_eq!(c.pass(8200).step, Step::Merge);
    // An authenticated but unsupported later word cannot revive an older Land.
    let unsupported = key.decision(REMI, 3, "autoland-judgment", "unsupported", &c.scope(), 11);
    c.hub.flow_comment(REMI, 3, "owner", &unsupported);
    assert!(c.facts().judgment.is_none());
    assert_eq!(c.pass(8300).step, Step::Judge(vec![Trigger::Asked]));
    let last = pass(12);
    c.hub.flow_comment(REMI, 3, "owner", &last);
    assert_eq!(c.pass(8400).step, Step::Merge);
}
#[test]
fn legacy_body_only_seals_are_not_upgraded_to_full_scope() {
    let mut c = Case::new();
    let scope = c.scope();
    let key = SealKey::from_hex(&"a".repeat(64)).unwrap();
    c.settings.judge_key = Some(key.clone());
    c.settings.verdict_authors.insert("owner".into());
    let tag = key.seal(
        REMI,
        3,
        "autoland-judgment",
        "land",
        &scope.head,
        &format!("main {}", fingerprint("")),
    );
    c.hub.flow_comment(
        REMI,
        3,
        "owner",
        &format!("<!-- autoland-judgment: land {} {tag} -->", scope.head),
    );
    assert_eq!(c.view(8000).step, Step::Judge(vec![Trigger::Asked]));
}

#[test]
fn mixed_proof_kinds_use_case_insensitive_membership_and_configured_plan_names() {
    use std::collections::BTreeMap;
    for selected in ["Build", "Run"] {
        let mut c = Case::new();
        c.settings.proofs = BTreeMap::from([
            ("Build".into(), Recipe::Build("checks.build".into())),
            ("Run".into(), Recipe::Run(vec!["check".into()])),
            ("VM".into(), Recipe::Job("exercise".into())),
        ]);
        c.hub.put_statuses(
            &c.scope().head,
            json!([
                {"context":"BUILD","state":if selected=="Build"{"pending"}else{"success"}},
                {"context":"rUN","state":if selected=="Run"{"pending"}else{"success"}},
                {"context":"vM","state":if selected=="VM"{"pending"}else{"success"}}
            ]),
        );
        let n = c.view(8000);
        assert_eq!(n.step, Step::Prepare);
        let recipes = BTreeMap::from([(selected.into(), c.settings.proofs[selected].clone())]);
        assert_eq!(n.proof, Some(ProofPlan::Prepare(recipes)));
    }
    let mut c = Case::new();
    c.settings
        .proofs
        .insert("VM".into(), Recipe::Run(vec!["exercise".into()]));
    c.hub.put_checks(
        &c.scope().head,
        json!([
            {"id":1,"name":"ci","status":"completed","conclusion":"success"},
            {"id":2,"name":"vM","status":"in_progress","conclusion":null}
        ]),
    );
    assert_eq!(c.view(8000).step, Step::Prepare);
}

#[test]
fn judgments_bind_the_complete_claim_after_a_fresh_review_of_an_edit() {
    for title in [false, true] {
        let mut c = Case::new();
        c.judge("pass");
        c.hub.edit_pull(REMI, 3, |p| {
            p[if title { "title" } else { "body" }] = json!("a new claim")
        });
        c.hub.refresh_flow(REMI, 3);
        c.review("reviewer", Verdict::Clean, 700);
        assert!(c.facts().judgment.is_none());
        assert_eq!(c.view(8000).step, Step::Judge(vec![Trigger::Asked]));
        c.judge("pass");
        assert_eq!(c.view(8000).step, Step::Merge);
    }
}

#[test]
fn rejected_sweep_remains_blocking_after_minor_reclassification() {
    for conflict in [false, true] {
        let mut c = Case::new();
        c.hub.edit_pull(REMI, 3, |p| {
            p["head"]["ref"] = json!("renovate/pkg");
            p["title"] = json!("Update pkg from 1 to 2");
        });
        c.hub.refresh_flow(REMI, 3);
        c.review("reviewer", Verdict::Clean, 600);
        c.sweep("held");
        c.pass(8000);
        let key = SealKey::from_hex(&"b".repeat(64)).unwrap();
        let marker = key.decision(
            REMI,
            3,
            "major-verdict",
            if conflict { "ok" } else { "unsupported" },
            &c.scope(),
            if conflict { 1 } else { 2 },
        );
        let at = c.hub.comments(REMI, 3).unwrap().len();
        c.hub.flow_comment(REMI, 3, "owner", &marker);
        c.label(&["security"]);
        for time in [8100, 8200] {
            assert!(!matches!(c.pass(time).step, Step::Merge | Step::JoinTrain));
            c.hub.edit_flow_comment(REMI, 3, at, "");
        }
        c.sweep("ok");
        c.sweep("ok");
        assert_eq!(c.pass(9000).step, Step::Merge);
    }
}
