use super::flow_case::Case;
use crate::flow::*;
use crate::sim::REMI;
use serde_json::json;
#[test]
fn an_outages_underway_look_counts_only_on_its_own_base() {
    // handle.py:329: an old-base look is another change, not a verdict still owed here.
    let mut c = Case::new();
    c.roles(&["available", "out"]);
    c.review("available", Verdict::Clean, 600);
    let mut old = c.scope();
    old.base = "release".into();
    c.service("out").unavailable_until = Some(86400);
    c.service("out").under_way_on = Some(old);
    assert_eq!(c.view(700).step, Step::Merge);
    let mut old = c.scope();
    old.head = "old-head".into();
    c.service("out").under_way_on = Some(old);
    assert_eq!(c.view(800).stage, Stage::Review);
    assert!(matches!(c.view(800).step, Step::Wait { .. }));
}
#[test]
fn returned_answers_post_before_a_new_description_is_reviewed() {
    // handle.py:58: an in-flight answer is posted on what it read, then the edited description needs a new look.
    let mut c = Case::new();
    c.roles(&["reviewer"]);
    let reviewed = c.scope();
    c.service("reviewer").answer = Some(Review {
        revision: reviewed,
        verdict: Verdict::Clean,
        at: 600,
    });
    c.description("Why not Rust: quicker to write", 650);
    assert_eq!(c.pass(650).step, Step::PostReview("reviewer".into()));
    c.service("reviewer").answer = None;
    assert_eq!(c.pass(700).step, Step::QueueReview("reviewer".into()));
    c.review("reviewer", Verdict::Clean, 710);
    assert_eq!(c.pass(720).step, Step::Merge);
}
#[test]
fn a_clean_new_head_covers_a_late_available_answer_but_docs_still_post_it() {
    // handle.py:112 / 592: a new look supersedes old findings; docs get no new look, so findings must be posted.
    for docs in [false, true] {
        let mut c = Case::new();
        c.roles(&["reviewer"]);
        c.service("reviewer").answer = Some(Review {
            revision: c.scope(),
            verdict: Verdict::Findings,
            at: 600,
        });
        c.push("head-after-look");
        if docs {
            c.hub.touch(&c.scope().head, &["docs/a.md"]);
        } else {
            c.review("reviewer", Verdict::Clean, 690);
        }
        assert_eq!(
            c.view(700).step,
            if docs {
                Step::PostReview("reviewer".into())
            } else {
                Step::Merge
            }
        );
    }
}
#[test]
fn a_judge_already_asked_for_still_decides_when_ci_appears_or_description_changes() {
    // handle.py:800 / 849: ordinary CI does not pass a judge already asked on this head and base.
    let mut c = Case::new();
    c.no_ci();
    c.pass(0);
    assert_eq!(c.pass(700).step, Step::Judge);
    c.jobs.insert(
        Kind::Judge,
        Job {
            id: "judge".into(),
            revision: c.scope(),
            state: JobState::Queued,
            started: 700,
            repeated: false,
        },
    );
    c.ci("success");
    assert!(matches!(c.pass(800).step, Step::Wait { .. }));
    c.description("its new description", 850);
    c.review("reviewer", Verdict::Clean, 851);
    assert_eq!(c.pass(900).step, Step::Judge);
}
#[test]
fn legacy_base_only_seals_owe_a_judge_but_are_not_verdicts_on_the_description() {
    // handle.py:739: old seals keep the judgment owed, never permission to land.
    for verdict in ["land", "owner", "failed"] {
        let mut c = Case::new();
        let key = SealKey::from_hex(&"a".repeat(64)).unwrap();
        c.settings.judge_key = Some(key.clone());
        c.settings.verdict_authors.insert("owner".into());
        let scope = c.scope();
        let tag = key.seal(REMI, 3, "autoland-judgment", verdict, &scope.head, "main");
        c.hub.flow_comment(
            REMI,
            3,
            "owner",
            &format!("<!-- autoland-judgment: {verdict} {} {tag} -->", scope.head),
        );
        assert_eq!(c.view(900).step, Step::Judge);
    }
}
#[test]
fn owner_label_holds_before_red_pending_ci_or_threads() {
    // handle.py:679: needs-owner takes precedence over every ordinary fix/review path.
    for scenario in 0..3 {
        let c = Case::new();
        c.label(&["needs-peter"]);
        match scenario {
            0 => c.ci("failure"),
            1 => c.hub.put_checks(
                &c.scope().head,
                json!([{"id":1,"name":"ci","status":"in_progress","conclusion":null}]),
            ),
            _ => c.threads(1),
        }
        assert_eq!(c.view(7200).step, Step::ListOwner);
    }
}
#[test]
fn documentation_policy_waives_only_the_review_round_not_findings_or_ci() {
    // handle.py:526 / 533 / 571: docs-only can land, mixed/hidden/policy code must be reviewed.
    for (paths, review) in [
        (&["docs/a.md", "README.md"][..], false),
        (&["docs/a.md", "src/a.rs"][..], true),
        (&["docs/AGENTS.md"][..], true),
        (&["docs/DESIGN.md"][..], true),
        (&["hidden/a.md"][..], true),
    ] {
        let mut c = Case::new();
        c.roles(&["reviewer"]);
        c.hub.touch(&c.scope().head, paths);
        assert_eq!(
            c.view(700).step,
            if review {
                Step::QueueReview("reviewer".into())
            } else {
                Step::Merge
            }
        );
    }
    for red in [false, true] {
        let mut c = Case::new();
        c.roles(&["reviewer"]);
        c.hub.touch(&c.scope().head, &["docs/a.md"]);
        if red {
            c.ci("failure");
        } else {
            c.threads(1);
        }
        assert_eq!(
            c.view(7200).step,
            Step::HandFix(if red { Kind::Red } else { Kind::Findings })
        );
    }
}
#[test]
fn ordinary_workflow_changes_are_reviewed_and_land_without_an_r2_gate() {
    // handle.py:370 / gates.py:9: .github alone is ordinary code; one-way paths come from policy.
    let c = Case::new();
    c.hub
        .touch(&c.scope().head, &[".github/workflows/ci.yml", "src/app.rs"]);
    assert_eq!(c.view(700).step, Step::Merge);
}
#[test]
fn a_cut_file_list_requires_judgment_and_cannot_keep_a_bots_ci_exemption() {
    // handle.py:507 / gates.py:97: paths beyond the readable list may hide an R2 or gate edit.
    let c = Case::new();
    let names: Vec<_> = (0..300).map(|i| format!("src/{i}.rs")).collect();
    let refs: Vec<_> = names.iter().map(String::as_str).collect();
    c.hub.touch(&c.scope().head, &refs);
    assert_eq!(c.facts().classified.class, conary_core::Class::R2);
    assert_eq!(c.view(700).step, Step::Judge);
}
#[test]
fn build_recipe_wins_when_case_variants_name_the_same_check() {
    // proving.py:550: a check configured as both job and build belongs to the build alone.
    let mut c = Case::new();
    c.settings
        .proofs
        .insert("VM".into(), Recipe::Job("run it".into()));
    c.settings
        .proofs
        .insert("vm".into(), Recipe::Build("checks.vm".into()));
    assert_eq!(c.settings.machine().len(), 1);
    assert!(
        matches!(c.view(700).proof,Some(ProofPlan::Evaluate(ref checks)) if checks.len()==1 && matches!(checks["vm"],Recipe::Build(_)))
    );
}

#[test]
fn confirmed_label_application_can_be_released_between_two_reads() {
    // handle.py:470: the executor recorded a successful label write, then the owner removed it before the next read.
    let mut c = Case::new();
    c.judge("owner");
    assert_eq!(c.pass(700).step, Step::ListOwner);
    c.memory.owner_saw = Some(c.scope()); // Confirmed executor observation, not an optimistic next() effect.
    assert_eq!(c.pass(710).step, Step::Merge);
    c.push("new-head-after-persons-answer");
    c.review("reviewer", Verdict::Clean, 720);
    c.judge("owner");
    assert_eq!(c.view(900).step, Step::ListOwner);
}
#[test]
fn the_newest_sealed_judge_verdict_for_the_revision_wins() {
    // handle.py:486: land after owner on this head supersedes it, but owner after land holds again.
    let mut c = Case::new();
    c.judge("owner");
    assert_eq!(c.pass(700).step, Step::ListOwner);
    c.judge("land");
    assert_eq!(c.pass(800).step, Step::Merge);
    c.judge("owner");
    assert_eq!(c.pass(900).step, Step::ListOwner);
}
#[test]
fn missing_job_proof_statuses_still_run_even_without_other_ci_or_on_a_dependency() {
    // handle.py:789 / 917: absent is pending for configured machine checks, never a no-CI judge.
    for (ci, bot) in [(true, false), (false, false), (true, true)] {
        let mut c = Case::new();
        c.settings
            .proofs
            .insert("proof".into(), Recipe::Job("run proof".into()));
        if !ci {
            c.no_ci();
        }
        if bot {
            c.hub
                .edit_pull(REMI, 3, |p| p["head"]["ref"] = json!("dependabot/pkg-2"));
            c.hub.refresh_flow(REMI, 3);
        }
        assert_eq!(c.view(900).step, Step::Prove);
        assert!(matches!(c.view(900).proof, Some(ProofPlan::Job(_))));
    }
}
#[test]
fn failed_proofs_get_their_own_fix_after_the_findings_job_ends() {
    // handle.py:862 / 872 / 907: a red proof is a new fix kind, serialized behind a findings job.
    for bot in [false, true] {
        let mut c = Case::new();
        c.settings
            .proofs
            .insert("proof".into(), Recipe::Job("run proof".into()));
        if bot {
            c.hub
                .edit_pull(REMI, 3, |p| p["head"]["ref"] = json!("dependabot/pkg-2"));
            c.hub.refresh_flow(REMI, 3);
        }
        c.hub.put_statuses(
            &c.scope().head,
            json!([{"context":"proof","state":"failure"}]),
        );
        c.jobs.insert(
            Kind::Findings,
            Job {
                id: "findings".into(),
                revision: c.scope(),
                state: JobState::Running,
                started: 700,
                repeated: false,
            },
        );
        assert!(matches!(c.pass(7200).step, Step::Wait { .. }));
        c.jobs.get_mut(&Kind::Findings).unwrap().state = JobState::Done;
        assert_eq!(c.pass(7300).step, Step::HandFix(Kind::Red));
    }
}
#[test]
fn a_major_renovate_proof_waits_for_its_sweeps_verdict() {
    // handle.py:922: no second proof job beside renovate-sweep before its ok.
    let mut c = Case::new();
    c.settings
        .proofs
        .insert("proof".into(), Recipe::Job("run proof".into()));
    c.hub.edit_pull(REMI, 3, |p| {
        p["head"]["ref"] = json!("renovate/pkg-2");
        p["title"] = json!("Update pkg from 1.4 to 2.0");
    });
    c.hub.refresh_flow(REMI, 3);
    assert!(matches!(c.view(900).step, Step::Wait { .. }));
    let key = SealKey::from_hex(&"b".repeat(64)).unwrap();
    c.settings.major_key = Some(key.clone());
    c.settings.verdict_authors.insert("owner".into());
    let scope = c.scope();
    let tag = key.seal(
        &REMI.to_lowercase(),
        3,
        "major-verdict",
        "ok",
        &scope.head,
        "main",
    );
    c.hub.flow_comment(
        REMI,
        3,
        "owner",
        &format!("<!-- major-verdict: ok {} on main {tag} -->", scope.head),
    );
    assert_eq!(c.view(1000).step, Step::Prove);
}
#[test]
fn an_unread_check_page_does_not_land_on_its_green_first_page() {
    // handle.py:978: something beyond the readable check page could be red.
    let c = Case::new();
    c.hub.edit_flow(REMI, 3, |s| s.checks_complete = false);
    let n = c.view(700);
    assert_eq!(n.stage, Stage::Ci);
    assert!(matches!(n.step, Step::Wait { .. }));
    assert!(n.description.contains("over 100 checks"));
}
#[test]
fn red_negative_and_other_named_checks_request_the_same_ci_fix_kind() {
    // handle.py:890: the negative's explanatory brief is executor formatting; its decision is a red-CI fix.
    for name in ["check / negative", "negative", "check / non-negative", "ci"] {
        let c = Case::new();
        c.hub.ci(&c.scope().head, &[(name, "failure")]);
        assert_eq!(c.view(7200).step, Step::HandFix(Kind::Red));
    }
}

#[test]
fn stacked_prs_wait_through_findings_red_ci_and_age_then_review_after_retarget() {
    // stacked.py:12 / 24 / 32: none of these facts starts work until the PR is retargeted; release branches are ordinary bases.
    for scenario in 0..4 {
        let mut c = Case::new();
        c.roles(&["reviewer"]);
        c.hub.pr(REMI, 2, &super::head(2));
        c.hub
            .edit_pull(REMI, 2, |p| p["head"]["ref"] = json!("below"));
        c.hub.set_branch(REMI, "below", super::TRUNK);
        c.hub.retarget(REMI, 3, "below");
        c.hub.refresh_flow(REMI, 3);
        match scenario {
            0 => c.review("reviewer", Verdict::Clean, 600),
            1 => {}
            2 => c.threads(1),
            _ => c.ci("failure"),
        }
        for at in [0, 700, 2 * 86400] {
            let n = c.pass(at);
            assert_eq!(n.stage, Stage::Base);
            assert!(matches!(n.step, Step::Wait { .. }));
            assert_eq!(n.overdue_at, None);
            assert_eq!(n.owner_notice, None);
        }
        c.hub.retarget(REMI, 3, "main");
        c.hub.refresh_flow(REMI, 3);
        c.threads(0);
        c.ci("success");
        assert_eq!(
            c.pass(2 * 86400 + 1).step,
            Step::QueueReview("reviewer".into())
        );
        c.review("reviewer", Verdict::Clean, 2 * 86400 + 2);
        assert_eq!(c.pass(2 * 86400 + 3).step, Step::Merge);
        c.hub.set_branch(REMI, "release/1.x", super::TRUNK);
        c.hub.retarget(REMI, 3, "release/1.x");
        c.hub.refresh_flow(REMI, 3);
        c.review("reviewer", Verdict::Clean, 2 * 86400 + 4);
        assert_eq!(c.view(2 * 86400 + 5).step, Step::Merge);
    }
}

#[test]
fn only_the_configured_stewards_rest_login_can_supply_a_review_mark() {
    // handle.py:356 / post.py:73: a person with the App's GraphQL name is not its REST [bot] identity.
    for (who, kind, accepted) in [
        ("fieldmouseworks-steward[bot]", "Bot", true),
        ("mallory", "User", false),
        ("fieldmouseworks-steward", "User", false),
    ] {
        let mut c = Case::new();
        c.roles(&["reviewer"]);
        let scope = c.scope();
        let body = format!(
            "<!-- reviewer-review:v1 {} -->",
            json!({"commit_sha":scope.head,"base":"main","description":fingerprint(""),"findings":0})
        );
        c.hub.put_reviews(REMI,3,json!([{"id":1,"user":{"login":who,"type":kind},"body":body,"commit_id":scope.head,"state":"COMMENTED","submitted_at":"2026-11-08T12:00:00Z"}]));
        assert_eq!(
            c.view(700).step,
            if accepted {
                Step::Merge
            } else {
                Step::QueueReview("reviewer".into())
            }
        );
    }
}
#[test]
fn a_mark_with_an_invalid_time_stops_the_reader() {
    // post.py:122: a malformed verdict timestamp is an error, never clean coverage.
    let c = Case::new();
    let scope = c.scope();
    let body = format!(
        "<!-- reviewer-review:v1 {} -->",
        json!({"commit_sha":scope.head,"base":"main","description":fingerprint(""),"findings":0})
    );
    c.hub.put_reviews(REMI,3,json!([{"id":1,"user":{"login":"fieldmouseworks-steward[bot]","type":"Bot"},"body":body,"commit_id":scope.head,"state":"COMMENTED","submitted_at":"yesterday"}]));
    assert!(
        read(
            &c.hub,
            &REMI.parse().unwrap(),
            3,
            &conary_core::Policy::parse(b"{}").unwrap(),
            c.settings.clone(),
            &["owner".into()]
        )
        .is_err()
    );
}
#[test]
fn reader_keeps_the_description_it_bound_for_the_future_executor() {
    // handle.py:449 / review.py:277: a task must receive exactly the PR text read, never a later refetch.
    let c = Case::new();
    c.description("The exact text whose verdict will be sealed.", 700);
    let f = c.facts();
    assert_eq!(
        f.description,
        "The exact text whose verdict will be sealed."
    );
    assert_eq!(f.number, 3);
    assert_eq!(f.repo.to_string(), REMI);
    c.description("A later edit", 701);
    assert_ne!(c.facts().description, f.description);
}
