//! Regression sequences for the review of ff9099e on #266.
use super::flow_case::Case;
use crate::api::Source;
use crate::flow::*;
use crate::sim::REMI;
use serde_json::json;
fn landing(step: &Step) -> bool {
    matches!(step, Step::Merge | Step::JoinTrain)
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
            let first = c.hub.comments(REMI, 3).unwrap().len();
            c.sweep("held");
            let n = c.pass(1000);
            assert_eq!(n.step, Step::Wait { until: None });
            assert!(n.description.starts_with("Major update held"), "{n:?}");
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
                c.hub.edit_flow_comment(REMI, 3, first + index, "");
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
        sized: crate::flow::durations::Sized::configured(10_800),
    }
}
fn holder(record: &ProofRecord) -> ProofHolder {
    ProofHolder {
        number: 3,
        job: record.job.clone(),
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
            let mut b = record(&c, "B", 200);
            c.proof_data.live.push(holder(&b));
            if completed {
                b.job.state = JobState::Done;
                c.proof_data.live.last_mut().unwrap().job.state = JobState::Done;
            }
            c.proof_data.records.push(b);
            assert_eq!(c.view(700).step, Step::StopJob("A".into())); // A proves an earlier head
            if recipe_change {
                c.settings
                    .proofs
                    .insert("VM".into(), Recipe::Build("checks.changed".into()));
            } else {
                c.push("proof-third-head");
                c.review("reviewer", Verdict::Clean, 800);
            }
            assert_eq!(c.pass(1000).step, Step::StopJob("A".into()));
            assert_eq!(c.pass(1100).step, Step::StopJob("A".into())); // until confirmed stopped
            c.stopped_jobs.insert("A".into());
            if !completed {
                assert_eq!(c.pass(1200).step, Step::StopJob("B".into()));
                c.stopped_jobs.insert("B".into()); // retained Running records must be retired
            }
            assert_eq!(c.pass(1300).step, Step::Prepare);
        }
    }
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
        let n = c.pass(1000);
        if own {
            assert_eq!(n.step, Step::StopJob("pool".into()));
        } else {
            assert_eq!(n.step, Step::Prepare);
        }
    }
}

#[test]
fn equal_review_timestamps_do_not_let_a_clean_summary_supersede_findings() {
    // Nothing the connector posts orders two of its posts in one second, so
    // a clean summary beside a findings review isn't clean.
    let mut c = Case::new();
    c.review("reviewer", Verdict::Findings, 900);
    c.review("reviewer", Verdict::Clean, 900);
    assert!(!landing(&c.pass(1000).step));
    c.review("reviewer", Verdict::Clean, 901);
    assert!(landing(&c.pass(1100).step));
}
