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
            c.judge("land");
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
            c.judge("land");
            assert_eq!(c.facts().judgment.unwrap().verdict, Judgment::Land);
        }
    }
}
#[test]
fn a_title_edit_invalidates_reviews_and_judgments_without_rebinding_legacy_scope() {
    for judged in [false, true] {
        let mut c = Case::new();
        if judged {
            c.judge("land");
        }
        c.hub
            .edit_pull(REMI, 3, |p| p["title"] = json!("change: a different claim"));
        c.hub.refresh_flow(REMI, 3);
        let f = c.facts();
        assert!(f.reviewers["reviewer"].review.is_none());
        if judged {
            assert!(f.judgment.is_none());
        }
        assert_eq!(c.view(8000).step, Step::QueueReview("reviewer".into()));
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
            c.judge("land");
            c.roles(&["reviewer"]);
            assert_eq!(c.view(8000).step, Step::QueueReview("reviewer".into()));
            c.review("reviewer", Verdict::Findings, 600);
            assert_eq!(c.view(8000).step, Step::HandFix(Kind::Findings));
            c.review("reviewer", Verdict::Clean, 700);
            let rev = c.scope();
            c.service("reviewer").answer = Some(Review {
                revision: rev.clone(),
                verdict: Verdict::Findings,
                at: 750,
            });
            assert_eq!(c.view(8000).step, Step::PostReview("reviewer".into()));
            c.service("reviewer").answer = None;
            c.memory
                .lost_on
                .entry("reviewer".into())
                .or_default()
                .insert("main".into(), rev.head.into());
            c.service("reviewer").unavailable_until = Some(9000);
            assert_eq!(c.view(8000).stage, Stage::Reviewer);
        }
    }
}
#[test]
fn later_findings_on_an_equivalent_head_precede_an_older_carried_clean_review() {
    let mut c = Case::new();
    c.review("reviewer", Verdict::Clean, 100);
    super::flow_carried::merge_base_into(&c, false);
    c.review("reviewer", Verdict::Findings, 200);
    super::flow_carried::merge_base_into(&c, false);
    c.pass(0);
    assert_eq!(c.view(8000).step, Step::HandFix(Kind::Findings));
    c.review("reviewer", Verdict::Clean, 300);
    assert_eq!(c.view(8000).step, Step::Merge);
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
            .insert("VM".into(), Recipe::Job("exercise".into()));
        if let Some(context) = context {
            c.hub.put_statuses(
                &c.scope().head,
                json!([{"context":context,"state":"pending"}]),
            );
        }
        assert_eq!(c.view(8000).step, Step::Prove);
        assert_eq!(
            c.view(8000).proof,
            Some(ProofPlan::Job(c.settings.proofs.clone()))
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
            c.judge("land");
        }
        let old = c.hub.comments(REMI, 3).unwrap()[0].body.clone().unwrap();
        if sweep {
            c.sweep("held");
        } else {
            c.judge("owner");
        }
        assert_eq!(c.pass(8000).step, Step::ListOwner);
        c.hub.edit_flow_comment(REMI, 3, 1, &old);
        let n = c.pass(8100);
        assert_eq!(n.step, Step::ListOwner); // Keep the authenticated Owner/Held decision, not just its counter.
        assert!(!matches!(
            n.step,
            Step::Merge | Step::JoinTrain | Step::Settle
        ));
        if sweep {
            c.sweep("ok");
        } else {
            c.judge("land");
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
    let land = key.decision(REMI, 3, "autoland-judgment", "land", &c.scope(), 9);
    let tampered = land.replace("\"generation\":9", "\"generation\":99");
    c.hub.flow_comment(REMI, 3, "owner", &tampered);
    assert!(c.facts().judgment.is_none());
    c.hub.flow_comment(REMI, 3, "owner", &land);
    let owner = key.decision(REMI, 3, "autoland-judgment", "owner", &c.scope(), 9);
    c.hub.flow_comment(REMI, 3, "owner", &owner);
    assert_eq!(c.pass(8000).step, Step::Judge);
    c.hub.edit_flow_comment(REMI, 3, 2, "");
    assert_eq!(c.pass(8100).step, Step::Judge);
    let newer = key.decision(REMI, 3, "autoland-judgment", "land", &c.scope(), 10);
    c.hub.flow_comment(REMI, 3, "owner", &newer);
    assert_eq!(c.pass(8200).step, Step::Merge);
    // An authenticated but unsupported later word cannot revive an older Land.
    let unsupported = key.decision(REMI, 3, "autoland-judgment", "unsupported", &c.scope(), 11);
    c.hub.flow_comment(REMI, 3, "owner", &unsupported);
    assert!(c.facts().judgment.is_none());
    assert_eq!(c.pass(8300).step, Step::Judge);
    let last = key.decision(REMI, 3, "autoland-judgment", "land", &c.scope(), 12);
    c.hub.flow_comment(REMI, 3, "owner", &last);
    assert_eq!(c.pass(8400).step, Step::Merge);
}
#[test]
fn legacy_body_only_review_marks_and_seals_are_not_upgraded_to_full_scope() {
    let mut c = Case::new();
    let scope = c.scope();
    let mark = json!({"commit_sha":scope.head,"base":scope.base,"description":fingerprint(""),"findings":0});
    c.hub.put_reviews(REMI,3,json!([{"id":99,"user":{"login":crate::sim::STEWARD,"type":"Bot"},"body":format!("<!-- reviewer-review:v1 {mark} -->"),"commit_id":scope.head,"submitted_at":"1970-01-01T00:00:00Z"}]));
    assert_eq!(c.view(8000).step, Step::QueueReview("reviewer".into()));
    c.review("reviewer", Verdict::Clean, 600);
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
    assert_eq!(c.view(8000).step, Step::Judge);
}
#[test]
fn carried_clean_provenance_cannot_cover_later_losses_or_refused_findings() {
    for refused in [false, true] {
        let mut c = Case::new();
        c.review("reviewer", Verdict::Clean, 100);
        let original = c.scope();
        super::flow_carried::merge_base_into(&c, false);
        c.pass(0);
        let middle = c.scope();
        if !refused {
            c.memory
                .lost_on
                .entry("reviewer".into())
                .or_default()
                .insert("main".into(), middle.head.clone().into());
            c.service("reviewer").unavailable_until = Some(10000);
        } else {
            c.service("reviewer").answer = Some(Review {
                revision: middle.clone(),
                verdict: Verdict::Findings,
                at: 900,
            });
            c.memory
                .review_posts
                .insert(("reviewer".into(), middle, 900), 3);
        }
        super::flow_carried::merge_base_into(&c, false);
        c.pass(0);
        assert_eq!(
            c.facts().reviewers["reviewer"].reviewed_head,
            Some(original.head)
        );
        let n = c.pass(8000);
        assert!(!matches!(
            n.step,
            Step::Merge | Step::JoinTrain | Step::Settle
        ));
        if refused {
            assert_eq!(n.step, Step::QueueReview("reviewer".into()));
        } else {
            assert_eq!(n.stage, Stage::Reviewer);
        }
    }
}
#[test]
fn carried_reviews_also_bind_title_and_body_to_the_original_claim() {
    for title in [false, true] {
        let c = Case::new();
        super::flow_carried::merge_base_into(&c, false);
        c.hub.edit_pull(REMI, 3, |p| {
            p[if title { "title" } else { "body" }] = json!("a different claim")
        });
        c.hub.refresh_flow(REMI, 3);
        assert!(c.facts().reviewers["reviewer"].review.is_none());
        assert!(!matches!(
            c.view(8000).step,
            Step::Merge | Step::JoinTrain | Step::Settle
        ));
    }
}

#[test]
fn mixed_proof_kinds_use_case_insensitive_membership_and_configured_plan_names() {
    use std::collections::BTreeMap;
    for selected in ["Build", "Run", "VM"] {
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
        assert_eq!(n.step, Step::Prove);
        let recipes = BTreeMap::from([(selected.into(), c.settings.proofs[selected].clone())]);
        assert_eq!(
            n.proof,
            Some(match selected {
                "Build" => ProofPlan::Evaluate(recipes),
                "Run" => ProofPlan::Run(recipes),
                _ => ProofPlan::Job(recipes),
            })
        );
    }
    let mut c = Case::new();
    c.settings
        .proofs
        .insert("VM".into(), Recipe::Job("exercise".into()));
    c.hub.put_checks(
        &c.scope().head,
        json!([
            {"id":1,"name":"ci","status":"completed","conclusion":"success"},
            {"id":2,"name":"vM","status":"in_progress","conclusion":null}
        ]),
    );
    assert_eq!(c.view(8000).step, Step::Prove);
}

#[test]
fn judgments_bind_the_complete_claim_after_a_fresh_review_of_an_edit() {
    for title in [false, true] {
        let mut c = Case::new();
        c.judge("land");
        c.hub.edit_pull(REMI, 3, |p| {
            p[if title { "title" } else { "body" }] = json!("a new claim")
        });
        c.hub.refresh_flow(REMI, 3);
        c.review("reviewer", Verdict::Clean, 700);
        assert!(c.facts().judgment.is_none());
        assert_eq!(c.view(8000).step, Step::Judge);
        c.judge("land");
        assert_eq!(c.view(8000).step, Step::Merge);
    }
}

#[test]
fn legacy_findings_keep_the_round_cap_without_supplying_clean_coverage() {
    let mut c = Case::new();
    c.roles(&["reviewer"]);
    let scope = c.scope();
    let mark = json!({"commit_sha":scope.head,"base":scope.base,"description":fingerprint(""),"findings":1});
    let reviews:Vec<_>=(1..=5).map(|id|json!({"id":id,"user":{"login":crate::sim::STEWARD,"type":"Bot"},"body":format!("<!-- reviewer-review:v1 {mark} -->"),"commit_id":scope.head,"submitted_at":"1970-01-01T00:00:00Z"})).collect();
    c.hub.put_reviews(REMI, 3, json!(reviews));
    let f = c.facts();
    assert_eq!(f.reviewers["reviewer"].rounds, 5);
    assert!(f.reviewers["reviewer"].review.is_none());
    assert_eq!(c.view(8000).step, Step::Judge);
    c.review("reviewer", Verdict::Clean, 600);
    assert_eq!(c.view(8000).step, Step::Merge);
}

#[test]
fn older_or_ambiguous_clean_cannot_discard_newer_refused_findings() {
    for carried in [false, true] {
        for clean_at in [100, 900, 901] {
            let mut c = Case::new();
            c.review("reviewer", Verdict::Clean, clean_at);
            let revision = c.scope();
            c.service("reviewer").answer = Some(Review {
                revision: revision.clone(),
                verdict: Verdict::Findings,
                at: 900,
            });
            c.memory
                .review_posts
                .insert(("reviewer".into(), revision, 900), 3);
            if carried {
                super::flow_carried::merge_base_into(&c, false);
            }
            let n = c.pass(8000);
            assert_eq!(
                matches!(n.step, Step::Merge | Step::JoinTrain | Step::Settle),
                clean_at > 900
            );
            if clean_at <= 900 {
                c.review("reviewer", Verdict::Clean, 8001);
                assert_eq!(c.pass(9000).step, Step::Merge);
            }
        }
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
        c.hub.flow_comment(REMI, 3, "owner", &marker);
        c.label(&["security"]);
        for time in [8100, 8200] {
            assert!(!matches!(
                c.pass(time).step,
                Step::Merge | Step::JoinTrain | Step::Settle
            ));
            c.hub.edit_flow_comment(REMI, 3, 1, "");
        }
        c.sweep("ok");
        c.sweep("ok");
        assert_eq!(c.pass(9000).step, Step::Merge);
    }
}

#[test]
fn a_different_rest_id_is_not_ordering_evidence_for_a_loss() {
    let mut c = Case::new();
    c.review("reviewer", Verdict::Clean, 100);
    let revision = c.scope();
    c.service("reviewer").answer = Some(Review {
        revision: revision.clone(),
        verdict: Verdict::Findings,
        at: 900,
    });
    c.memory
        .review_posts
        .insert(("reviewer".into(), revision, 900), 3);
    c.pass(8000);
    c.service("reviewer").answer = None;
    for time in [99, 899, 900] {
        c.review("reviewer", Verdict::Clean, time);
        assert!(!matches!(
            c.pass(8100).step,
            Step::Merge | Step::JoinTrain | Step::Settle
        ));
    }
    c.review("reviewer", Verdict::Clean, 901);
    assert_eq!(c.pass(8200).step, Step::Merge);
}
