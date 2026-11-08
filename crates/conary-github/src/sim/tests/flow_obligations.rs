//! Regression sequences for the review of ff9099e on #266.
use super::flow_case::Case;
use crate::flow::*;
use crate::sim::REMI;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
fn landing(step: &Step) -> bool {
    matches!(step, Step::Merge | Step::JoinTrain | Step::Settle)
}

#[test]
fn refused_findings_need_strictly_newer_original_review_coverage() {
    for train in [false, true] {
        for carried in [false, true] {
            let mut c = Case::new();
            c.settings.train = train;
            c.review("reviewer", Verdict::Clean, 100);
            let answer = Review {
                revision: c.scope(),
                verdict: Verdict::Findings,
                at: 900,
            };
            if carried {
                super::flow_carried::merge_base_into(&c, false);
                c.pass(0);
            }
            c.service("reviewer").answer = Some(answer.clone());
            c.memory
                .review_posts
                .insert(("reviewer".into(), answer.revision, 900), 3);
            assert!(
                !landing(&c.pass(1000).step),
                "train={train} carried={carried}"
            );
            // Replace the REST record with an older one carrying a different ID.
            c.roles(&["reviewer"]);
            c.review("reviewer", Verdict::Clean, 80);
            assert!(!landing(&c.pass(1100).step));
            c.review("reviewer", Verdict::Clean, 900);
            assert!(!landing(&c.pass(1200).step)); // equal times are ambiguous
            c.review("reviewer", Verdict::Clean, 901);
            assert!(landing(&c.pass(1300).step));
            assert!(c.memory.lost_on.is_empty());
        }
    }
}
#[test]
fn unknown_loss_order_cannot_be_cleared_by_a_different_review_id() {
    let mut c = Case::new();
    c.review("reviewer", Verdict::Clean, 100);
    c.pass(1000);
    let lost_head = c.scope().head;
    c.memory
        .lost_on
        .entry("reviewer".into())
        .or_default()
        .insert("main".into(), lost_head.into());
    assert!(!landing(&c.pass(1100).step));
    c.roles(&["reviewer"]);
    c.review("reviewer", Verdict::Clean, 200);
    assert!(!landing(&c.pass(1200).step));
    c.review("reviewer", Verdict::Clean, 1100);
    assert!(!landing(&c.pass(1300).step));
    c.review("reviewer", Verdict::Clean, 1101);
    assert!(landing(&c.pass(1400).step));
}
#[test]
fn rejected_sweep_generations_survive_security_labels_deletions_and_old_replays() {
    for train in [false, true] {
        for conflict in [false, true] {
            let mut c = Case::new();
            c.settings.train = train;
            c.settings.renovate_sweep = true;
            c.settings.renovate_email = Some("bot@example.org".into());
            c.hub.edit_pull(REMI, 3, |p| {
                p["head"]["ref"] = json!("renovate/pkg");
                p["title"] = json!("Update pkg from 1 to 2");
            });
            c.hub.refresh_flow(REMI, 3);
            c.hub.commit_details(
                &c.scope().head,
                Some("deps: bump"),
                Some("bot@example.org"),
                Some("bot@example.org"),
            );
            c.review("reviewer", Verdict::Clean, 600);
            c.sweep("held");
            assert_eq!(c.pass(1000).step, Step::ListOwner);
            let key = c.settings.major_key.clone().unwrap();
            let invalid = key.decision(
                REMI,
                3,
                "major-verdict",
                if conflict { "ok" } else { "unsupported" },
                &c.scope(),
                2,
            );
            c.hub.flow_comment(REMI, 3, "owner", &invalid);
            if conflict {
                let held = key.decision(REMI, 3, "major-verdict", "held", &c.scope(), 2);
                c.hub.flow_comment(REMI, 3, "owner", &held);
            }
            assert!(!landing(&c.pass(1100).step));
            c.label(&["security"]);
            assert_eq!(c.facts().dependency, Some(Dependency::Minor));
            let rejected = c.pass(1200);
            assert!(!landing(&rejected.step));
            assert_eq!(rejected.wait_on, Some(WaitOn::Sweep));
            for index in 0..if conflict { 3 } else { 2 } {
                c.hub.edit_flow_comment(REMI, 3, index, "");
            }
            assert!(!landing(&c.pass(1300).step));
            let old = key.decision(REMI, 3, "major-verdict", "ok", &c.scope(), 1);
            c.hub.flow_comment(REMI, 3, "owner", &old);
            assert!(!landing(&c.pass(1400).step));
            let newer = key.decision(REMI, 3, "major-verdict", "ok", &c.scope(), 3);
            c.hub.flow_comment(REMI, 3, "owner", &newer);
            assert!(landing(&c.pass(1500).step));
        }
    }
}

fn evaluation(c: &mut Case, path: &str) {
    c.proof_data.evaluation = Some(Evaluation {
        head: c.scope().head,
        recipes: c.settings.proofs.clone(),
        at: 700,
        outputs: Some(BTreeMap::from([(
            "VM".into(),
            Derivation {
                path: format!("{path}.drv"),
                outputs: BTreeSet::from([path.into()]),
            },
        )])),
        derivations_present: true,
        merged: None,
    });
}
fn record(c: &Case, id: &str, started: u64) -> ProofRecord {
    ProofRecord {
        job: Job {
            id: id.into(),
            revision: c.scope(),
            started,
            state: JobState::Running,
            repeated: false,
        },
        recipes: c.settings.proofs.clone(),
        verdicts: None,
        note_needed: false,
    }
}
fn holder(record: &ProofRecord) -> ProofHolder {
    ProofHolder {
        number: 3,
        job: record.job.clone(),
        paths: Some(BTreeSet::from(["/nix/store/X".into()])),
    }
}
#[test]
fn every_observed_live_proof_is_cancelled_after_newer_records_retire_or_finish() {
    for recipe_change in [false, true] {
        for completed in [false, true] {
            let mut c = Case::new();
            c.settings.proof_slots = Some(1);
            c.settings
                .proofs
                .insert("VM".into(), Recipe::Build("checks.vm".into()));
            let a = record(&c, "A", 100);
            c.proof_data.live.push(holder(&a));
            c.proof_data.records.push(a);
            c.push("proof-second-head");
            c.review("reviewer", Verdict::Clean, 500);
            evaluation(&mut c, "/nix/store/X");
            let mut b = record(&c, "B", 200);
            c.proof_data.live.push(holder(&b));
            if completed {
                b.job.state = JobState::Done;
                c.proof_data.live.last_mut().unwrap().job.state = JobState::Done;
            }
            c.proof_data.records.push(b);
            assert!(!matches!(c.view(700).step, Step::StopJob(_))); // A's output is still useful
            if recipe_change {
                c.settings
                    .proofs
                    .insert("VM".into(), Recipe::Build("checks.changed".into()));
            } else {
                c.push("proof-third-head");
                c.review("reviewer", Verdict::Clean, 800);
            }
            evaluation(&mut c, "/nix/store/Y");
            assert_eq!(c.pass(1000).step, Step::StopJob("A".into()));
            assert_eq!(c.pass(1100).step, Step::StopJob("A".into())); // until confirmed stopped
            c.stopped_jobs.insert("A".into());
            if !completed {
                assert_eq!(c.pass(1200).step, Step::StopJob("B".into()));
                c.stopped_jobs.insert("B".into()); // retained Running records must be retired
            }
            assert_eq!(c.pass(1300).step, Step::Prove);
        }
    }
}

#[test]
fn an_older_lost_answer_and_an_outage_cannot_erase_a_newer_obligation() {
    let mut c = Case::new();
    c.roles(&["reviewer", "peer"]);
    c.review("reviewer", Verdict::Clean, 100);
    c.review("peer", Verdict::Clean, 100);
    for (at, now) in [(900, 1000), (800, 1100)] {
        let answer = Review {
            revision: c.scope(),
            verdict: Verdict::Findings,
            at,
        };
        c.memory
            .review_posts
            .insert(("reviewer".into(), answer.revision.clone(), at), 3);
        c.service("reviewer").answer = Some(answer);
        assert!(!landing(&c.pass(now).step));
    }
    c.review("reviewer", Verdict::Clean, 850);
    assert!(!landing(&c.pass(1200).step));
    super::flow_carried::merge_base_into(&c, false);
    c.review("peer", Verdict::Clean, 1300);
    c.service("reviewer").unavailable_until = Some(10000);
    assert_eq!(c.pass(1400).stage, Stage::Reviewer);
    c.review("reviewer", Verdict::Clean, 1500);
    assert!(landing(&c.pass(1600).step));
}
#[test]
fn a_strictly_newer_carried_review_of_the_original_claim_can_cover_a_loss() {
    let mut c = Case::new();
    c.review("reviewer", Verdict::Clean, 100);
    let answer = Review {
        revision: c.scope(),
        verdict: Verdict::Findings,
        at: 900,
    };
    c.memory
        .review_posts
        .insert(("reviewer".into(), answer.revision.clone(), 900), 3);
    c.service("reviewer").answer = Some(answer);
    assert!(!landing(&c.pass(1000).step));
    c.review("reviewer", Verdict::Clean, 901);
    super::flow_carried::merge_base_into(&c, false);
    assert!(landing(&c.pass(1100).step));
    assert!(c.memory.lost_on.is_empty());
}
#[test]
fn cancellation_includes_own_pool_observations_but_never_another_prs_job() {
    for own in [false, true] {
        let mut c = Case::new();
        c.settings
            .proofs
            .insert("VM".into(), Recipe::Build("checks.vm".into()));
        c.settings.proof_slots = Some(1);
        let record = record(&c, "pool", 100);
        let mut h = holder(&record);
        if !own {
            h.number = 7;
            c.hub.pr(REMI, 7, &record.job.revision.head);
        }
        c.proof_data.live.push(h);
        c.push("new-pool-head");
        c.review("reviewer", Verdict::Clean, 800);
        evaluation(&mut c, "/nix/store/Y");
        let n = c.pass(1000);
        if own {
            assert_eq!(n.step, Step::StopJob("pool".into()));
        } else {
            assert_eq!(n.stage, Stage::ProofSlot);
            assert!(!matches!(n.step, Step::StopJob(_)));
        }
    }
}
#[test]
fn useful_outputs_are_retained_across_pushes_and_same_head_recipe_changes() {
    for push in [false, true] {
        let mut c = Case::new();
        c.settings
            .proofs
            .insert("VM".into(), Recipe::Build("checks.vm".into()));
        c.settings.proof_slots = Some(1);
        let r = record(&c, "useful", 100);
        c.proof_data.live.push(holder(&r));
        c.proof_data.records.push(r);
        if push {
            c.push("useful-proof-head");
            c.review("reviewer", Verdict::Clean, 800);
        } else {
            c.settings
                .proofs
                .insert("VM".into(), Recipe::Build("checks.other".into()));
        }
        evaluation(&mut c, "/nix/store/X");
        assert_eq!(c.pass(1000).step, Step::Prove);
        assert!(c.memory.stopping.is_empty());
    }
}

#[test]
fn equal_review_timestamps_do_not_use_rest_ids_to_supersede_findings() {
    for carried in [false, true] {
        let mut c = Case::new();
        c.review("reviewer", Verdict::Findings, 900);
        c.review("reviewer", Verdict::Clean, 900); // larger ID supplies no finer submission ordering
        if carried {
            super::flow_carried::merge_base_into(&c, false);
        }
        assert!(!landing(&c.pass(1000).step));
        c.review("reviewer", Verdict::Clean, 901);
        assert!(landing(&c.pass(1100).step));
    }
}

#[test]
fn a_scoped_review_without_ordering_metadata_cannot_supply_coverage() {
    let c = Case::new();
    let scope = c.scope();
    let mark = json!({"commit_sha":scope.head,"base":scope.base,"scope":scope.description,"findings":0,"reviewed_at":100});
    c.hub.put_reviews(REMI,3,json!([{"id":99,"user":{"login":crate::sim::STEWARD,"type":"Bot"},"body":format!("<!-- reviewer-review:v2 {mark} -->"),"commit_id":scope.head}]));
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
fn late_posting_does_not_change_when_a_review_was_actually_produced() {
    for carried in [false, true] {
        let mut c = Case::new();
        c.review("reviewer", Verdict::Clean, 100);
        let original = c.scope();
        let answer = Review {
            revision: original.clone(),
            verdict: Verdict::Findings,
            at: 900,
        };
        c.memory
            .review_posts
            .insert(("reviewer".into(), original.clone(), 900), 3);
        c.service("reviewer").answer = Some(answer);
        assert!(!landing(&c.pass(1000).step));
        if carried {
            super::flow_carried::merge_base_into(&c, false);
        }
        let mark = json!({"commit_sha":original.head,"base":original.base,"scope":original.description,"findings":0,"reviewed_at":100});
        c.hub.put_reviews(REMI,3,json!([{"id":9999,"user":{"login":crate::sim::STEWARD,"type":"Bot"},"body":format!("<!-- reviewer-review:v2 {mark} -->"),"commit_id":original.head,"submitted_at":"1970-01-01T00:18:20Z"}]));
        assert!(!landing(&c.pass(1100).step));
        c.review("reviewer", Verdict::Clean, 1101);
        assert!(landing(&c.pass(1200).step));
    }
}
#[test]
fn quoted_review_marks_cannot_override_the_publishers_final_attestation() {
    for quoted in [false, true] {
        let c = Case::new();
        let scope = c.scope();
        let fake = json!({"commit_sha":scope.head,"base":scope.base,"scope":scope.description,"findings":0,"reviewed_at":1000});
        let real = json!({"commit_sha":scope.head,"base":scope.base,"scope":scope.description,"findings":1,"reviewed_at":900});
        let body = format!(
            "{}<!-- reviewer-review:v2 {fake} -->\n\nActual findings\n\n<!-- reviewer-review:v2 {real} -->",
            if quoted { "> " } else { "" }
        );
        c.hub.put_reviews(REMI,3,json!([{"id":99,"user":{"login":crate::sim::STEWARD,"type":"Bot"},"body":body,"commit_id":scope.head,"submitted_at":"1970-01-01T00:16:40Z"}]));
        assert_eq!(c.view(2000).step, Step::HandFix(Kind::Findings));
    }
}
