//! Decision scenarios from burrow at 7841daf. Python has executable blocks,
//! not test functions; comments identify the block by file and opening text.
use std::collections::BTreeMap;

use super::flow_case::Case;
use super::{REMI, head, hub};
use crate::flow::*;
use crate::sim::Hub;
use serde_json::json;

pub(super) fn setup() -> (Hub, Facts, Memory) {
    let hub = hub();
    hub.pr(REMI, 3, &head(3));
    hub.edit_pull(REMI, 3, |p| p["user"]["login"] = json!("owner"));
    hub.ci(&head(3), &[("check", "success")]);
    hub.flow_snapshot(
        REMI,
        3,
        Snapshot {
            default_branch: None,
            revision: Revision::from(&crate::api::Source::pull(&hub, REMI, 3).unwrap()),
            draft: false,
            complete: true,
            merge_state: "CLEAN".into(),
            open_threads: 0,
            threads: Default::default(),
            resolved_reviews: Default::default(),
            updated_at: 0,
            suites: BTreeMap::new(),
            rebase_removed: Vec::new(),
            timeline_cut_at: None,
            since: Default::default(),
        },
    );
    hub.flow_review(REMI, 3, Verdict::Clean, 0);
    let f = read(
        &hub,
        &REMI.parse().unwrap(),
        3,
        &conary_core::Policy::parse(br#"{"r2":["^secrets/","CODEOWNERS"],"docs":["^docs/"]}"#)
            .unwrap(),
        crate::sim::settings(),
        &["owner".into()],
    )
    .unwrap();
    let m = Memory {
        head: head(3),
        seen: 0,
        ..Memory::default()
    };
    (hub, f, m)
}

#[test]
fn draft_precedes_outside_and_incomplete() {
    // handle.py: drafts precede outside authors and unreadable histories.
    let c = Case::new();
    c.hub.edit_flow(REMI, 3, |s| {
        s.draft = true;
        s.complete = false;
    });
    c.hub
        .edit_pull(REMI, 3, |p| p["user"]["login"] = json!("outside"));
    let n = c.view(4000);
    assert_eq!(n.stage, Stage::None);
    assert_eq!(n.overdue_at, None);
    assert_eq!(n.step, Step::Wait { until: None });
}

#[test]
fn outside_is_the_owners_and_unreadable_is_the_flows() {
    // handle.py listed both for the owner; only an outside author is a
    // one-way door. A cut history is the flow's own wait, overdue in its
    // status.
    let c = Case::new();
    c.hub.edit_flow(REMI, 3, |s| s.complete = false);
    let n = c.view(4000);
    assert_eq!((n.stage, n.step), (Stage::None, Step::Wait { until: None }));
    assert!(n.overdue_at.is_some());
    c.hub.edit_flow(REMI, 3, |s| s.complete = true);
    c.hub
        .edit_pull(REMI, 3, |p| p["user"]["login"] = json!("outside"));
    assert_eq!(c.view(4000).step, Step::ListOwner(Door::Outside));
}

#[test]
fn quiet_head_gets_one_review_intention() {
    // review_step.py: ask once the head has gone quiet, then wait on that ask.
    let mut c = Case::new();
    c.hub.remove_flow_reviews(REMI, 3);
    assert_eq!(c.view(1).step, Step::Wait { until: Some(1800) });
    assert!(matches!(c.view(4000).step, Step::AskReview(_)));
    c.memory.asked = Some((head(3), 4000));
    assert_eq!(c.view(4000).step, Step::Wait { until: Some(7600) });
}

#[test]
fn changed_description_does_not_reuse_review() {
    // reviews.rs: the connector's word binds only the commit, so a
    // description edited after the push goes to a judge.
    let c = Case::new();
    c.description("new reasons", 0);
    c.hub.edit_flow(REMI, 3, |s| s.since.edited = Some(100));
    assert_eq!(c.view(4000).step, Step::Judge(vec![Trigger::Changed]));
}

#[test]
fn clean_current_head_lands() {
    // handle.py: trusted clean marks on the current scope, no threads, green CI.
    assert_eq!(Case::new().view(4000).step, Step::Merge);
}

#[test]
fn open_thread_prevents_merge() {
    // handle.py: open threads block even clean reviewer verdicts.
    let c = Case::new();
    c.threads(1);
    assert_eq!(c.view(4000).step, Step::HandFix(Kind::Findings));
}

#[test]
fn current_changes_requested_prevents_merge() {
    // handle.py: changes-requested reviews count independently of inline threads.
    let c = Case::new();
    c.hub.put_reviews(
        REMI,
        3,
        json!([{"user":{"login":"human","type":"User"},"state":"CHANGES_REQUESTED"}]),
    );
    assert_eq!(c.view(4000).step, Step::HandFix(Kind::Findings));
}

#[test]
fn stale_head_verdict_cannot_land() {
    // handle.py: a review of an old commit cannot cover this head.
    let c = Case::new();
    c.push(&head(4));
    let n = c.view(4000);
    assert_eq!(
        (n.stage, n.step),
        (
            Stage::Review,
            Step::Wait {
                until: Some(4000 + 1800)
            }
        )
    );
}

#[test]
fn retargeted_verdict_cannot_land() {
    // reviews.rs: a review of the push cannot cover a diff against the base
    // it was retargeted to since.
    let c = Case::new();
    c.hub.edit_flow(REMI, 3, |s| {
        s.since = Since {
            edited: None,
            retargeted: Some(200),
            pushed: Some(100),
        }
    });
    assert_eq!(c.view(4000).step, Step::Judge(vec![Trigger::Changed]));
}

#[test]
fn r2_requires_person_even_after_judge_land() {
    // gates.py: a judge's land verdict never replaces the R2 person's answer.
    let mut c = Case::new();
    c.secret();
    c.judge("pass");
    assert_eq!(c.view(4000).step, Step::ListOwner(Door::R2));
}

#[test]
fn owner_approval_is_required_after_label_removal_and_is_revision_scoped() {
    // A label removal cannot release an owner decision; an approval is scoped.
    let mut c = Case::new();
    c.secret();
    c.judge("owner");
    c.label(&["needs-peter"]);
    assert_eq!(c.pass(4000).step, Step::ListOwner(Door::Label));
    c.label(&[]);
    assert_eq!(c.view(4001).step, Step::ListOwner(Door::Judge));
    c.approve();
    assert_eq!(c.view(4001).step, Step::Merge);
    c.description("different reasons", 4002);
    c.review("reviewer", Verdict::Clean, 4003);
    c.judge("owner");
    assert_eq!(c.view(5000).step, Step::ListOwner(Door::Judge));
}

#[test]
fn stale_judgment_is_not_a_verdict_for_this_head() {
    // gates.py: a judgment at an old head cannot release the new one.
    let mut c = Case::new();
    c.judge("pass");
    c.push(&head(4));
    c.secret();
    c.review("reviewer", Verdict::Clean, 1);
    assert_eq!(c.view(4000).step, Step::Judge(vec![Trigger::R2]));
}

#[test]
fn judge_failure_is_held_even_with_pending_ci() {
    // handle.py: a failing judgment holds it whatever CI is doing.
    let mut c = Case::new();
    c.judge("fail");
    c.hub.put_checks(&head(3),json!([{"id":1,"name":"check","status":"in_progress","conclusion":null,"check_suite":{"id":1}}]));
    assert_eq!(c.view(4000).step, Step::HandFix(Kind::JudgeFailure));
}

#[test]
fn unknown_authorship_waits() {
    // codex_heads.py: unread history neither lands nor spends a judge.
    let c = Case::new();
    c.hub.fail(crate::sim::Call::CompareCommits);
    assert_eq!(c.view(4000).stage, Stage::Review);
    assert!(!landing(&c.view(4000).step));
}

#[test]
fn every_merge_state_has_a_step() {
    // strikes.py: every documented mergeStateStatus and an unknown value.
    let c = Case::new();
    for state in ["CLEAN", "HAS_HOOKS", "UNSTABLE", "BLOCKED"] {
        c.hub.edit_flow(REMI, 3, |s| s.merge_state = state.into());
        assert_eq!(c.view(4000).step, Step::Merge);
    }
    c.hub
        .edit_flow(REMI, 3, |s| s.merge_state = "BEHIND".into());
    assert_eq!(c.view(4000).step, Step::UpdateBranch);
    c.hub.edit_flow(REMI, 3, |s| s.merge_state = "DIRTY".into());
    assert_eq!(c.view(4000).step, Step::HandFix(Kind::Conflict));
    for state in ["UNKNOWN", "DRAFT", "SOMETHING_NEW"] {
        c.hub.edit_flow(REMI, 3, |s| s.merge_state = state.into());
        assert_eq!(
            c.view(4000).step,
            Step::Retry {
                kind: Failure::Mergeable,
                at: 4060
            }
        );
    }
}

#[test]
fn strikes_back_off_and_third_is_overdue() {
    // strikes.py: one, two, four minutes; third failure is overdue.
    let mut c = Case::new();
    c.failed = Some(Failure::Merge);
    for (now, delay) in [(4000, 60), (5000, 120), (6000, 240)] {
        let n = c.pass(now);
        assert_eq!(
            n.step,
            Step::Retry {
                kind: Failure::Merge,
                at: now + delay
            }
        );
        if now == 6000 {
            assert!(n.overdue_at.unwrap() <= now);
        }
    }
    c.failed = None;
    assert_eq!(
        c.view(6001).step,
        Step::Retry {
            kind: Failure::Merge,
            at: 6240
        }
    );
}

#[test]
fn push_resets_strikes() {
    // strikes.py: counters are per head.
    let mut c = Case::new();
    c.memory.strikes.insert(Failure::Merge, 3);
    c.push(&head(4));
    c.pass(5000);
    assert_eq!(c.memory.seen, 5000);
    assert!(c.memory.strikes.is_empty());
}

#[test]
fn stacked_pr_is_not_reviewed_or_landed() {
    // stacked.py: the PR below is discovered from GitHub's open PRs.
    let c = Case::new();
    c.hub.pr(REMI, 2, &head(2));
    c.hub
        .edit_pull(REMI, 2, |p| p["head"]["ref"] = json!("below"));
    c.hub.set_branch(REMI, "below", super::TRUNK);
    c.hub.retarget(REMI, 3, "below");
    c.hub.refresh_flow(REMI, 3);
    let n = c.view(4000);
    assert_eq!(n.stage, Stage::Base);
    assert_eq!(n.overdue_at, None);
}

#[test]
fn hold_is_unstaged_but_watched() {
    // waiting.py / watched.py: hold is unstaged and has its own deadline.
    let mut c = Case::new();
    c.label(&["hold"]);
    let n = c.pass(4000);
    assert_eq!(n.stage, Stage::None);
    assert_eq!(n.overdue_at, Some(4000 + c.settings.hold_stall_after));
    assert_eq!(c.view(5000).overdue_at, n.overdue_at);
}

#[test]
fn same_wait_keeps_its_deadline() {
    // watched.py: same phase and revision keep their first deadline.
    let mut c = Case::new();
    c.hub.put_checks(
        &head(3),
        json!([{"id":1,"name":"check","status":"in_progress","conclusion":null}]),
    );
    let n = c.pass(4000);
    assert_eq!(c.view(5000).overdue_at, n.overdue_at);
    c.hub.set_branch(REMI, "release", super::TRUNK);
    c.hub.retarget(REMI, 3, "release");
    c.hub.refresh_flow(REMI, 3);
    assert_ne!(c.view(5000).overdue_at, n.overdue_at);
}

#[test]
fn empty_change_closes_but_bot_closes_its_own() {
    // landed.py: the result tree is empty; dependency bots close their own.
    let c = Case::new();
    let tip = c.hub.add(&head(3), "after");
    c.hub.set_branch(REMI, "main", &tip);
    assert_eq!(c.view(4000).step, Step::CloseAlreadyLanded);
    c.hub
        .edit_pull(REMI, 3, |p| p["head"]["ref"] = json!("dependabot/pkg"));
    assert_eq!(c.view(4000).step, Step::Wait { until: None });
}

#[test]
fn settled_train_pr_joins_despite_unknown_mergeability() {
    // trains.py: the candidate, not GitHub's isolated merge, decides a train PR.
    let mut c = Case::new();
    c.settings.train = true;
    c.hub
        .edit_flow(REMI, 3, |s| s.merge_state = "UNKNOWN".into());
    assert_eq!(c.view(4000).step, Step::JoinTrain);
}

#[test]
fn missing_ci_waits_five_minutes_then_needs_a_judge() {
    // handle.py: a fresh head gets five minutes to show its first CI check.
    let mut c = Case::new();
    c.no_ci();
    assert_eq!(c.pass(4000).step, Step::Wait { until: Some(4300) });
    assert_eq!(c.view(4300).step, Step::Judge(vec![Trigger::NoCi]));
}

#[test]
fn ci_not_required_does_not_need_a_judge() {
    // handle.py: a repository configured not to require CI needs none.
    let mut c = Case::new();
    c.no_ci();
    c.settings.require_ci = false;
    assert_eq!(c.view(4000).step, Step::Merge);
}

#[test]
fn proof_slot_running_and_unposted_are_distinct() {
    // proving.py / waiting.py: these are local machine observations, not GitHub.
    let mut c = Case::new();
    c.settings
        .proofs
        .insert("proof".into(), Recipe::Run(vec!["run proof".into()]));
    c.programs();
    for (proof, stage, step) in [
        (
            Proof::InLine(2),
            Stage::ProofSlot,
            Step::Wait { until: None },
        ),
        (Proof::Running, Stage::Proof, Step::Wait { until: None }),
        (Proof::Unposted, Stage::Proof, Step::PostProof),
        (Proof::Needed, Stage::Proof, Step::Prove),
    ] {
        c.proof = Some(proof);
        let n = c.view(4000);
        assert_eq!((n.stage, n.step), (stage, step));
    }
}

#[test]
fn red_ci_handoff_waits_for_session_quiet() {
    // handle.py / unpushed.py: pushes/replies update GitHub's updatedAt quiet.
    let c = Case::new();
    c.ci("failure");
    c.description("https://claude.ai/code/session_test", 3000);
    c.hub.flow_review(REMI, 3, Verdict::Clean, 3001);
    assert_eq!(c.view(4000).step, Step::Wait { until: Some(10201) });
    assert_eq!(c.view(10201).step, Step::HandFix(Kind::Red));
}

#[test]
fn only_one_fix_job_owns_the_branch_across_pushes() {
    // unpushed.py / conflicts.py: a fix job owns the branch across pushes.
    let mut c = Case::new();
    c.threads(1);
    let mut old = c.scope();
    old.head = head(2);
    c.jobs.insert(
        Kind::Red,
        Job {
            id: "fix".into(),
            revision: old,
            state: JobState::Running,
            started: 0,
            repeated: false,
        },
    );
    assert_eq!(c.view(4000).step, Step::Wait { until: Some(4900) });
}

#[test]
fn broken_job_is_seen_twice_then_backed_off_once() {
    // lost.py / strikes.py: one missing answer may race its producer.
    let mut c = Case::new();
    c.secret();
    c.jobs.insert(
        Kind::Judge,
        Job {
            id: "j".into(),
            revision: c.scope(),
            state: JobState::Broken,
            started: 0,
            repeated: false,
        },
    );
    assert_eq!(c.pass(4000).step, Step::Wait { until: Some(4060) });
    assert_eq!(
        c.pass(4060).step,
        Step::Retry {
            kind: Failure::Lost,
            at: 4120
        }
    );
    c.pass(4070);
    assert_eq!(c.memory.strikes[&Failure::Lost], 1);
    assert_eq!(c.view(4120).step, Step::Judge(vec![Trigger::R2]));
}

#[test]
fn two_completed_jobs_without_verdict_stall_without_the_owner() {
    // lost.py: two completed unanswered turns, not broken jobs, reached the
    // owner. They're no door: no third job starts, and the wait is overdue.
    let mut c = Case::new();
    c.secret();
    c.jobs.insert(
        Kind::Judge,
        Job {
            id: "j".into(),
            revision: c.scope(),
            state: JobState::Done,
            started: 0,
            repeated: true,
        },
    );
    c.pass(4000);
    let n = c.view(4001);
    assert_eq!(n.step, Step::Wait { until: None });
    assert_eq!(n.overdue_at, Some(4001));
}

#[test]
fn dependency_major_waits_for_sweep_and_red_is_sweeps() {
    // deps.py: sweep's major verdict precedes proofs/judgment.
    let mut c = Case::new();
    c.hub
        .edit_pull(REMI, 3, |p| p["head"]["ref"] = json!("renovate/pkg"));
    c.label(&["major"]);
    c.proof = Some(Proof::Needed);
    assert_eq!(c.view(4000).stage, Stage::Review);
    c.ci("failure");
    assert_eq!(c.view(4000).stage, Stage::Fix);
}

#[test]
fn dependency_held_and_release_age() {
    // deps.py / waiting.py: sealed held major and Renovate's lock-age comment.
    let mut c = Case::new();
    c.hub
        .edit_pull(REMI, 3, |p| p["head"]["ref"] = json!("renovate/pkg"));
    c.label(&["major"]);
    c.sweep("held");
    // The sweep names no door, so its hold is no listing.
    assert_eq!(c.view(4000).step, Step::Wait { until: None });
    c.settings.major_key = None;
    c.settings.renovate_login = Some("renovate[bot]".into());
    c.hub.put_checks(&head(3), json!([]));
    c.hub.post_status(
        &head(3),
        json!({"context":"renovate/artifacts","state":"failure"}),
    );
    c.hub.flow_comment(REMI,3,"renovate[bot]","Artifact update problem\n```\nlock-age: nothing in the refresh is old enough until 1970-01-01T01:40:00Z\n```\n");
    assert_eq!(c.view(4000).stage, Stage::ReleaseAge);
}

#[test]
fn stage_strings_match_python() {
    // waiting.py: stages.py's exact keys are the label keys.
    for (stage, value) in [
        (Stage::Review, json!("review")),
        (Stage::Fix, json!("fix")),
        (Stage::Ci, json!("ci")),
        (Stage::ProofSlot, json!("proofSlot")),
        (Stage::Proof, json!("proof")),
        (Stage::ReleaseAge, json!("releaseAge")),
        (Stage::Train, json!("train")),
        (Stage::Merge, json!("merge")),
        (Stage::Owner, json!("owner")),
        (Stage::Base, json!("base")),
        (Stage::None, json!(null)),
    ] {
        assert_eq!(serde_json::to_value(stage).unwrap(), value);
        assert_eq!(serde_json::from_value::<Stage>(value).unwrap(), stage);
    }
}

#[test]
fn reader_refuses_a_push_between_endpoints() {
    let (hub, _, _) = setup();
    hub.push(REMI, 3, &head(4));
    let result = read(
        &hub,
        &REMI.parse().unwrap(),
        3,
        &conary_core::Policy::parse(b"{}").unwrap(),
        crate::sim::settings(),
        &["owner".into()],
    );
    assert!(result.unwrap_err().to_string().contains("moved"));
}

#[test]
fn reader_failure_is_not_a_clean_snapshot() {
    let (hub, _, _) = setup();
    hub.fail(crate::sim::Call::FlowSnapshot);
    assert!(
        read(
            &hub,
            &REMI.parse().unwrap(),
            3,
            &conary_core::Policy::parse(b"{}").unwrap(),
            crate::sim::settings(),
            &["owner".into()]
        )
        .is_err()
    );
}

#[test]
fn reader_uses_latest_run_and_ignores_own_status() {
    // cancelled.py / passes.py: reruns replace stopped or failed checks.
    let (hub, _, _) = setup();
    hub.ci(&head(3), &[("check", "failure"), ("check", "success")]);
    let f = read(
        &hub,
        &REMI.parse().unwrap(),
        3,
        &conary_core::Policy::parse(b"{}").unwrap(),
        crate::sim::settings(),
        &["owner".into()],
    )
    .unwrap();
    assert_eq!(
        f.checks,
        [Check {
            name: "check".into(),
            state: Ci::Green,
            run: Place::Unplaced
        }]
    );
}

fn landing(step: &Step) -> bool {
    matches!(step, Step::Merge | Step::JoinTrain)
}

fn read_checks(hub: &Hub) -> Vec<Check> {
    read(
        hub,
        &REMI.parse().unwrap(),
        3,
        &conary_core::Policy::parse(b"{}").unwrap(),
        crate::sim::settings(),
        &["owner".into()],
    )
    .unwrap()
    .checks
}

#[test]
fn unknown_provenance_blocks_every_landing_path() {
    // review finding: branch identity cannot replace a complete provenance read.
    for (branch, train) in [
        ("feature", false),
        ("feature", true),
        ("renovate/pkg", false),
        ("renovate/pkg", true),
        ("dependabot/pkg", false),
        ("dependabot/pkg", true),
    ] {
        let mut c = Case::new();
        c.settings.train = train;
        c.hub
            .edit_pull(REMI, 3, |p| p["head"]["ref"] = json!(branch));
        c.hub.fail(crate::sim::Call::CompareCommits);
        assert!(!landing(&c.view(4000).step));
    }
}

#[test]
fn only_known_bot_history_skips_review() {
    // review finding: only a read history of bots alone gets CI-only review.
    for renovate in [true, false] {
        let mut c = Case::new();
        c.hub.remove_flow_reviews(REMI, 3);
        let email = if renovate {
            "renovate@example.org"
        } else {
            "49699333+dependabot[bot]@users.noreply.github.com"
        };
        if renovate {
            c.settings.renovate_email = Some(email.into());
        }
        c.hub.edit_pull(REMI, 3, |p| {
            p["head"]["ref"] = json!(if renovate {
                "renovate/pkg"
            } else {
                "dependabot/pkg"
            })
        });
        c.hub
            .commit_details(&head(3), Some("deps: bump"), Some(email), Some(email));
        assert_eq!(c.view(4000).step, Step::Merge);
        c.hub.fail(crate::sim::Call::CompareCommits);
        assert!(matches!(c.view(4000).step, Step::AskReview(_)));
        c.hub.heal(crate::sim::Call::CompareCommits);
        c.hub.commit_details(
            &head(3),
            Some("change: code"),
            Some("owner@example.org"),
            Some(email),
        );
        assert!(matches!(c.view(4000).step, Step::AskReview(_)));
    }
}

#[test]
fn same_named_checks_of_other_suites_keep_their_failures() {
    // gate.rs: a run gives way only to a rerun in its own suite.
    let (hub, _, _) = setup();
    let run = |id: u64, suite: Option<u64>, status: &str, conclusion: Option<&str>| {
        let mut v = json!({"id": id, "name": "check", "status": status,
                           "conclusion": conclusion});
        if let Some(suite) = suite {
            v["check_suite"] = json!({"id": suite});
        }
        v
    };
    let states = |hub: &Hub| -> Vec<Ci> { read_checks(hub).into_iter().map(|c| c.state).collect() };

    // Another workflow's later success leaves this one's failure red.
    hub.put_checks(
        &head(3),
        json!([
            run(1, Some(10), "completed", Some("failure")),
            run(2, Some(20), "completed", Some("success"))
        ]),
    );
    assert!(states(&hub).contains(&Ci::Red));
    // And its pending run pending.
    hub.put_checks(
        &head(3),
        json!([
            run(1, Some(10), "in_progress", None),
            run(2, Some(20), "completed", Some("success"))
        ]),
    );
    assert!(states(&hub).contains(&Ci::Pending));
    // Runs with no suite to tell them apart replace nothing.
    hub.put_checks(
        &head(3),
        json!([
            run(1, None, "completed", Some("failure")),
            run(2, None, "completed", Some("success"))
        ]),
    );
    assert!(states(&hub).contains(&Ci::Red));
    hub.put_checks(
        &head(3),
        json!([
            run(1, Some(10), "completed", Some("failure")),
            run(2, None, "completed", Some("success"))
        ]),
    );
    assert!(states(&hub).contains(&Ci::Red));
    // A rerun in its own suite does replace it, whichever order GitHub lists.
    hub.put_checks(
        &head(3),
        json!([
            run(2, Some(10), "completed", Some("success")),
            run(1, Some(10), "completed", Some("failure"))
        ]),
    );
    assert_eq!(states(&hub), [Ci::Green]);
}

#[test]
fn other_suites_failure_prevents_landing() {
    // review finding: another suite's success cannot erase an independent red.
    let c = Case::new();
    c.hub.put_checks(&head(3),json!([{"id":1,"name":"check","status":"completed","conclusion":"failure","check_suite":{"id":10}},{"id":2,"name":"check","status":"completed","conclusion":"success","check_suite":{"id":20}}]));
    assert_eq!(c.view(4000).step, Step::HandFix(Kind::Red));
}

#[test]
fn r2_land_is_released_by_the_owner_for_its_revision_only() {
    // Labels never authorize; an authenticated approval covers only its revision.
    for change in 0..3 {
        let mut c = Case::new();
        c.secret();
        c.judge("pass");
        assert_eq!(c.pass(4000).step, Step::ListOwner(Door::R2));
        assert!(!c.facts().owner_approved);
        assert_eq!(c.pass(4001).step, Step::ListOwner(Door::R2));
        c.label(&["needs-peter"]);
        c.pass(4002);
        c.label(&[]);
        assert_eq!(c.view(4003).step, Step::ListOwner(Door::R2));
        c.approve();
        assert_eq!(c.view(4003).step, Step::Merge);
        match change {
            0 => c.push(&head(4)),
            1 => {
                c.hub.set_branch(REMI, "release", super::TRUNK);
                c.hub.retarget(REMI, 3, "release");
                c.hub.refresh_flow(REMI, 3);
            }
            _ => c.description("different reasons", 4004),
        }
        c.secret();
        c.review("reviewer", Verdict::Clean, 4005);
        c.judge("pass");
        assert_eq!(c.view(5000).step, Step::ListOwner(Door::R2));
    }
}

#[test]
fn label_without_an_owed_decision_is_not_approval() {
    // review finding: a label before a verdict, or on R0 Land, is not approval.
    let mut c = Case::new();
    c.secret();
    c.label(&["needs-peter"]);
    c.pass(4000);
    assert!(!c.facts().owner_approved);
    c.hub.touch(&head(3), &["src/a.rs"]);
    c.judge("pass");
    c.pass(4001);
    assert!(!c.facts().owner_approved);
}
