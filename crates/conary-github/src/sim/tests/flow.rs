//! Decision scenarios from burrow at 7841daf. Python has executable blocks,
//! not test functions; comments identify the block by file and opening text.
use std::collections::BTreeMap;

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
            revision: Revision::from(&crate::api::Source::pull(&hub, REMI, 3).unwrap()),
            draft: false,
            complete: true,
            merge_state: "CLEAN".into(),
            open_threads: 0,
            changes_requested: false,
            reviewers: BTreeMap::new(),
            updated_at: 0,
            suites: BTreeMap::new(),
        },
    );
    let mut f = read(
        &hub,
        &REMI.parse().unwrap(),
        3,
        &conary_core::Policy::parse(br#"{"r2":["^secrets/","CODEOWNERS"],"docs":["^docs/"]}"#)
            .unwrap(),
        Settings::default(),
        &["owner".into()],
    )
    .unwrap();
    // These GitHub facts are not exposed by the initial reader adapter yet.
    f.authorship_known = true;
    f.empty = Some(false);
    f.reviewers.insert(
        "reviewer".into(),
        Reviewer {
            review: Some(Review {
                revision: f.revision.clone(),
                verdict: Verdict::Clean,
                at: 0,
            }),
            ..Reviewer::default()
        },
    );
    let m = Memory {
        head: head(3),
        seen: 0,
        ..Memory::default()
    };
    (hub, f, m)
}

fn step(f: &Facts, m: &Memory) -> Step {
    next(f, m, 4000).0.step
}

#[test]
fn draft_precedes_outside_and_incomplete() {
    // handle.py: "A draft is work in progress"; "Whoever's it is".
    let (_, mut f, m) = setup();
    f.draft = true;
    f.trusted = false;
    f.complete = false;
    let (n, _) = next(&f, &m, 4000);
    assert_eq!(n.stage, Stage::None);
    assert_eq!(n.overdue_at, None);
    assert_eq!(n.step, Step::Wait { until: None });
}

#[test]
fn outside_and_unreadable_are_the_owners() {
    // handle.py: "Ready, more comments than one page could hide a verdict".
    let (_, mut f, m) = setup();
    f.complete = false;
    assert_eq!(step(&f, &m), Step::ListOwner);
    f.complete = true;
    f.trusted = false;
    assert_eq!(step(&f, &m), Step::ListOwner);
}

#[test]
fn quiet_head_gets_one_review_intention() {
    // handle.py: "A head is reviewed by both once it has sat still".
    let (_, mut f, m) = setup();
    f.reviewers.get_mut("reviewer").unwrap().review = None;
    assert_eq!(next(&f, &m, 1).0.step, Step::Wait { until: Some(300) });
    assert_eq!(step(&f, &m), Step::QueueReview("reviewer".into()));
    f.reviewers.get_mut("reviewer").unwrap().queued = true;
    assert_eq!(step(&f, &m), Step::Wait { until: None });
}

#[test]
fn changed_description_does_not_reuse_review() {
    // handle.py: "A review covers the description it was shown".
    let (_, mut f, m) = setup();
    f.revision.description.push_str("edited");
    assert_eq!(step(&f, &m), Step::QueueReview("reviewer".into()));
}

#[test]
fn clean_current_head_lands() {
    // handle.py: "Their answers are posted by the next pass, which lands".
    let (_, f, m) = setup();
    assert_eq!(step(&f, &m), Step::Merge);
}

#[test]
fn open_thread_prevents_merge() {
    // handle.py: "Open threads, clean or not" (findings handoff).
    let (_, mut f, m) = setup();
    f.open_threads = 1;
    assert_eq!(step(&f, &m), Step::HandFix(Kind::Findings));
}

#[test]
fn current_changes_requested_prevents_merge() {
    // handle.py: changes-requested reviews are handed with open threads.
    let (_, mut f, m) = setup();
    f.changes_requested = true;
    assert_eq!(step(&f, &m), Step::HandFix(Kind::Findings));
}

#[test]
fn stale_head_verdict_cannot_land() {
    // handle.py: "A look at an earlier head ... can't know what ... fixed".
    let (_, mut f, m) = setup();
    f.reviewers
        .get_mut("reviewer")
        .unwrap()
        .review
        .as_mut()
        .unwrap()
        .revision
        .head = head(2);
    assert_eq!(step(&f, &m), Step::QueueReview("reviewer".into()));
}

#[test]
fn retargeted_verdict_cannot_land() {
    // carried.py: a review of a different base does not cover this change.
    let (_, mut f, m) = setup();
    f.reviewers
        .get_mut("reviewer")
        .unwrap()
        .review
        .as_mut()
        .unwrap()
        .revision
        .base = "other".into();
    assert_eq!(step(&f, &m), Step::QueueReview("reviewer".into()));
}

#[test]
fn answer_is_posted_before_landing() {
    // post.py / handle.py: service answers are posted on the next pass.
    let (_, mut f, m) = setup();
    let r = f.reviewers.get_mut("reviewer").unwrap();
    r.answer = r.review.clone();
    assert_eq!(step(&f, &m), Step::PostReview("reviewer".into()));
}

#[test]
fn r2_requires_person_even_after_judge_land() {
    // gates.py: one-way paths belong to a person; judge cannot grant authority.
    let (_, mut f, m) = setup();
    f.classified.class = conary_core::Class::R2;
    f.judgment = Some(Judged {
        revision: f.revision.clone(),
        verdict: Judgment::Land,
    });
    assert_eq!(step(&f, &m), Step::ListOwner);
}

#[test]
fn owner_label_seen_and_removed_is_revision_scoped() {
    // handle.py: "the owner has a label to take off ... their answer".
    let (_, mut f, m) = setup();
    f.classified.class = conary_core::Class::R2;
    f.judgment = Some(Judged {
        revision: f.revision.clone(),
        verdict: Judgment::Owner,
    });
    f.labels.insert("needs-peter".into());
    let (n, m) = next(&f, &m, 4000);
    assert_eq!(n.step, Step::ListOwner);
    f.labels.clear();
    assert_eq!(step(&f, &m), Step::Merge);
    f.revision.description.push('!');
    assert_ne!(step(&f, &m), Step::Merge);
}

#[test]
fn stale_judgment_is_not_a_verdict_for_this_head() {
    // misdescribed.py / gates.py: judgment is for head, base and description.
    let (_, mut f, m) = setup();
    f.needs_judge = true;
    let mut old = f.revision.clone();
    old.head = head(2);
    f.judgment = Some(Judged {
        revision: old,
        verdict: Judgment::Land,
    });
    assert_eq!(step(&f, &m), Step::Judge);
}

#[test]
fn judge_failure_is_held_even_with_pending_ci() {
    // handle.py: "Its owner or failing verdict holds it whatever CI ... doing".
    let (_, mut f, m) = setup();
    f.checks[0].state = Ci::Pending;
    f.judgment = Some(Judged {
        revision: f.revision.clone(),
        verdict: Judgment::Failing,
    });
    assert_eq!(step(&f, &m), Step::HandFix(Kind::JudgeFailure));
}

#[test]
fn unknown_authorship_waits() {
    // codex_heads.py: an unreadable commit list neither lands nor spends a judge.
    let (_, mut f, m) = setup();
    f.authorship_known = false;
    assert_eq!(next(&f, &m, 4000).0.stage, Stage::Review);
    assert_ne!(step(&f, &m), Step::Merge);
}

#[test]
fn every_merge_state_has_a_step() {
    // strikes.py: "Every merge state GitHub's API has gets a step".
    let (_, mut f, m) = setup();
    for state in ["CLEAN", "HAS_HOOKS", "UNSTABLE", "BLOCKED"] {
        f.merging = Merge::from_github(state);
        assert_eq!(step(&f, &m), Step::Merge, "{state}");
    }
    f.merging = Merge::from_github("BEHIND");
    assert_eq!(step(&f, &m), Step::UpdateBranch);
    f.merging = Merge::from_github("DIRTY");
    assert_eq!(step(&f, &m), Step::HandFix(Kind::Conflict));
    for state in ["UNKNOWN", "DRAFT", "SOMETHING_NEW"] {
        f.merging = Merge::from_github(state);
        assert_eq!(step(&f, &m), Step::Wait { until: None });
    }
}

#[test]
fn strikes_back_off_and_third_is_overdue() {
    // strikes.py: "Every step ... a minute on, then two, then four".
    let (_, mut f, mut m) = setup();
    f.failed = Some(Failure::Merge);
    for (now, delay) in [(4000, 60), (5000, 120), (6000, 240)] {
        let (n, changed) = next(&f, &m, now);
        m = changed;
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
    f.failed = None;
    assert_eq!(
        next(&f, &m, 6001).0.step,
        Step::Retry {
            kind: Failure::Merge,
            at: 6240
        }
    );
}

#[test]
fn push_resets_strikes_but_keeps_lost_findings() {
    // strikes.py / superseded.py: per-head counters, durable lost findings.
    let (_, mut f, mut m) = setup();
    m.strikes.insert(Failure::Merge, 3);
    m.lost_on
        .insert("absent".into(), BTreeMap::from([("main".into(), head(2))]));
    f.revision.head = head(4);
    let (_, changed) = next(&f, &m, 5000);
    assert_eq!(changed.seen, 5000);
    assert!(changed.strikes.is_empty());
    assert_eq!(changed.lost_on, m.lost_on);
}

#[test]
fn lost_findings_from_unavailable_reviewer_hold_the_head() {
    // lost.py: findings that could not be posted survive a reviewer's outage.
    let (_, mut f, mut m) = setup();
    f.reviewers.get_mut("reviewer").unwrap().unavailable_until = Some(9000);
    m.lost_on.insert(
        "reviewer".into(),
        BTreeMap::from([(f.revision.base.clone(), head(3))]),
    );
    assert_eq!(next(&f, &m, 4000).0.stage, Stage::Reviewer);
}

#[test]
fn stacked_pr_is_not_reviewed_or_landed() {
    // stacked.py: a PR based on another open PR waits for retargeting.
    let (_, mut f, m) = setup();
    f.stacked_on = Some(2);
    let n = next(&f, &m, 4000).0;
    assert_eq!(n.stage, Stage::Base);
    assert_eq!(n.overdue_at, None);
}

#[test]
fn hold_is_unstaged_but_watched() {
    // waiting.py / watched.py: a hold keeps its own longer deadline.
    let (_, mut f, m) = setup();
    f.labels.insert("hold".into());
    let (n, m) = next(&f, &m, 4000);
    assert_eq!(n.stage, Stage::None);
    assert_eq!(n.overdue_at, Some(4000 + f.settings.hold_stall_after));
    assert_eq!(next(&f, &m, 5000).0.overdue_at, n.overdue_at);
}

#[test]
fn same_wait_keeps_its_deadline() {
    // watched.py: "A wait is the same one while its phase ... and rev are".
    let (_, mut f, m) = setup();
    f.checks[0].state = Ci::Pending;
    let (a, m) = next(&f, &m, 4000);
    assert_eq!(next(&f, &m, 5000).0.overdue_at, a.overdue_at);
    f.revision.base = "release".into();
    assert_ne!(next(&f, &m, 5000).0.overdue_at, a.overdue_at);
}

#[test]
fn empty_change_closes_but_bot_closes_its_own() {
    // landed.py: "a dependency bot's is left to its bot".
    let (_, mut f, m) = setup();
    f.empty = Some(true);
    assert_eq!(step(&f, &m), Step::CloseAlreadyLanded);
    f.dependency = Some(Dependency::Minor);
    f.bot_edited = Some(false);
    assert_eq!(step(&f, &m), Step::Wait { until: None });
}

#[test]
fn settled_train_pr_joins_despite_unknown_mergeability() {
    // trains.py: the train decides merging against its own candidate.
    let (_, mut f, m) = setup();
    f.settings.train = true;
    f.merging = Merge::Unknown;
    assert_eq!(step(&f, &m), Step::JoinTrain);
}

#[test]
fn missing_ci_waits_five_minutes_then_needs_a_judge() {
    // handle.py: "A head just pushed has no checks until GitHub ... started".
    let (_, mut f, m) = setup();
    f.checks.clear();
    let (a, m) = next(&f, &m, 4000);
    assert_eq!(a.stage, Stage::Ci);
    assert_eq!(a.step, Step::Wait { until: Some(4300) });
    assert_eq!(next(&f, &m, 4300).0.step, Step::Judge);
}

#[test]
fn ci_not_required_does_not_need_a_judge() {
    // handle.py: "Unless the repo is set not to need any (docs only)".
    let (_, mut f, m) = setup();
    f.checks.clear();
    f.settings.require_ci = false;
    assert_eq!(step(&f, &m), Step::Merge);
}

#[test]
fn proof_slot_running_and_unposted_are_distinct() {
    // proving.py / waiting.py: proof queued, running and verdict not posted.
    let (_, mut f, m) = setup();
    for (proof, stage, expected) in [
        (
            Proof::InLine(2),
            Stage::ProofSlot,
            Step::Wait { until: None },
        ),
        (Proof::Running, Stage::Proof, Step::Wait { until: None }),
        (Proof::Unposted, Stage::Proof, Step::PostProof),
        (Proof::Needed, Stage::Proof, Step::Prove),
    ] {
        f.proof = proof;
        let n = next(&f, &m, 4000).0;
        assert_eq!((n.stage, n.step), (stage, expected));
    }
}

#[test]
fn red_ci_handoff_waits_for_session_quiet() {
    // handle.py / unpushed.py: pushes and replies restart the handoff wait.
    let (_, mut f, m) = setup();
    f.checks[0].state = Ci::Red;
    f.session = true;
    f.last_reply_at = 3000;
    assert_eq!(step(&f, &m), Step::Wait { until: Some(10200) });
    assert_eq!(next(&f, &m, 10201).0.step, Step::HandFix(Kind::Red));
}

#[test]
fn only_one_fix_job_owns_the_branch_across_pushes() {
    // unpushed.py / conflicts.py: two fix jobs must not push over each other.
    let (_, mut f, m) = setup();
    f.open_threads = 1;
    let mut old = f.revision.clone();
    old.head = head(2);
    f.jobs.insert(
        Kind::Red,
        Job {
            id: "fix".into(),
            revision: old,
            state: JobState::Running,
            started: 0,
            repeated: false,
        },
    );
    assert_eq!(step(&f, &m), Step::Wait { until: None });
}

#[test]
fn broken_job_is_seen_twice_then_backed_off_once() {
    // lost.py / strikes.py: first missing answer may race the GitHub read.
    let (_, mut f, m) = setup();
    f.needs_judge = true;
    f.jobs.insert(
        Kind::Judge,
        Job {
            id: "j".into(),
            revision: f.revision.clone(),
            state: JobState::Broken,
            started: 0,
            repeated: false,
        },
    );
    let (n, m) = next(&f, &m, 4000);
    assert_eq!(n.step, Step::Wait { until: Some(4060) });
    let (n, m) = next(&f, &m, 4060);
    assert_eq!(
        n.step,
        Step::Retry {
            kind: Failure::Lost,
            at: 4120
        }
    );
    let (_, m) = next(&f, &m, 4070);
    assert_eq!(m.strikes[&Failure::Lost], 1);
    assert_eq!(next(&f, &m, 4120).0.step, Step::Judge);
}

#[test]
fn two_completed_jobs_without_verdict_go_to_owner() {
    // lost.py: broken jobs do not spend the two unanswered turns.
    let (_, mut f, m) = setup();
    f.needs_judge = true;
    f.jobs.insert(
        Kind::Judge,
        Job {
            id: "j".into(),
            revision: f.revision.clone(),
            state: JobState::Done,
            started: 0,
            repeated: true,
        },
    );
    let (_, m) = next(&f, &m, 4000);
    assert_eq!(step(&f, &m), Step::ListOwner);
}

#[test]
fn dependency_major_waits_for_sweep_and_red_is_sweeps() {
    // deps.py: major verdict comes before proofs or judgment.
    let (_, mut f, m) = setup();
    f.renovate = true;
    f.dependency = Some(Dependency::Major);
    f.proof = Proof::Needed;
    assert_eq!(next(&f, &m, 4000).0.stage, Stage::Review);
    f.checks[0].state = Ci::Red;
    assert_eq!(next(&f, &m, 4000).0.stage, Stage::Fix);
}

#[test]
fn dependency_held_and_release_age() {
    // deps.py: held majors stay the owner's; immature lock refresh waits.
    let (_, mut f, m) = setup();
    f.renovate = true;
    f.dependency = Some(Dependency::Major);
    f.major = Some(Major::Held);
    assert_eq!(step(&f, &m), Step::ListOwner);
    f.major = None;
    f.release_at = Some(6000);
    assert_eq!(next(&f, &m, 4000).0.stage, Stage::ReleaseAge);
}

#[test]
fn stage_strings_match_python() {
    // waiting.py: stages.py's exact keys are the label keys.
    for (stage, value) in [
        (Stage::Review, json!("review")),
        (Stage::Reviewer, json!("reviewer")),
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
        Settings::default(),
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
            Settings::default(),
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
        Settings::default(),
        &["owner".into()],
    )
    .unwrap();
    assert_eq!(
        f.checks,
        [Check {
            name: "check".into(),
            state: Ci::Green
        }]
    );
}

fn landing(step: &Step) -> bool {
    matches!(step, Step::Merge | Step::Settle | Step::JoinTrain)
}

fn read_checks(hub: &Hub) -> Vec<Check> {
    read(
        hub,
        &REMI.parse().unwrap(),
        3,
        &conary_core::Policy::parse(b"{}").unwrap(),
        Settings::default(),
        &["owner".into()],
    )
    .unwrap()
    .checks
}

#[test]
fn unknown_provenance_blocks_every_landing_path() {
    // codex_heads.py / touches.py: unread commits never land, bot's or not.
    let (_, mut f, m) = setup();
    f.authorship_known = false;
    for (renovate, dependency, bot_edited, train) in [
        (false, None, None, false),
        (false, None, None, true),
        (true, Some(Dependency::Minor), Some(false), false),
        (true, Some(Dependency::Minor), None, false),
        (true, Some(Dependency::Minor), Some(false), true),
        (false, Some(Dependency::Minor), Some(false), false),
        (false, Some(Dependency::Minor), None, false),
        (false, Some(Dependency::Minor), Some(false), true),
    ] {
        f.renovate = renovate;
        f.bot_merges = renovate;
        f.dependency = dependency;
        f.bot_edited = bot_edited;
        f.settings.train = train;
        let n = next(&f, &m, 4000).0;
        assert!(
            !landing(&n.step),
            "{renovate} {dependency:?} {bot_edited:?} {train}: {n:?}"
        );
    }
}

#[test]
fn only_known_bot_history_skips_review() {
    // touches.py: bot-only commits are CI-reviewed; anyone else's are read.
    let (_, mut f, m) = setup();
    f.reviewers.get_mut("reviewer").unwrap().review = None;
    f.renovate = true;
    f.bot_merges = true;
    f.dependency = Some(Dependency::Minor);
    f.bot_edited = Some(false);
    assert_eq!(step(&f, &m), Step::Settle);
    f.renovate = false;
    f.bot_merges = false;
    assert_eq!(step(&f, &m), Step::Merge);
    // A branch name alone, or an unread history, is not the bots' alone.
    for renovate in [true, false] {
        f.renovate = renovate;
        f.bot_merges = renovate;
        f.bot_edited = None;
        assert_eq!(step(&f, &m), Step::QueueReview("reviewer".into()));
        f.bot_edited = Some(true);
        assert_eq!(step(&f, &m), Step::QueueReview("reviewer".into()));
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
    let (hub, mut f, m) = setup();
    hub.put_checks(
        &head(3),
        json!([{"id": 1, "name": "check", "status": "completed", "conclusion": "failure",
                "check_suite": {"id": 10}},
               {"id": 2, "name": "check", "status": "completed", "conclusion": "success",
                "check_suite": {"id": 20}}]),
    );
    f.checks = read_checks(&hub);
    assert_eq!(step(&f, &m), Step::HandFix(Kind::Red));
}

#[test]
fn current_verdicts_stand_while_their_reviewer_is_unavailable() {
    // lost.py / carried.py: an outage neither drops findings nor coverage.
    let (_, mut f, m) = setup();
    let verdict = |f: &Facts, verdict| Reviewer {
        review: Some(Review {
            revision: f.revision.clone(),
            verdict,
            at: 0,
        }),
        unavailable_until: Some(9000),
        ..Reviewer::default()
    };
    // Current findings from the reviewer out of service still block.
    let absent = verdict(&f, Verdict::Findings);
    f.reviewers.insert("absent".into(), absent);
    assert_eq!(step(&f, &m), Step::HandFix(Kind::Findings));
    // Even when it is the only reviewer, and even while it is under way.
    f.reviewers.remove("reviewer");
    assert_eq!(step(&f, &m), Step::HandFix(Kind::Findings));
    f.reviewers.get_mut("absent").unwrap().under_way = true;
    assert!(!landing(&step(&f, &m)));
    // Its current clean verdict still covers the revision.
    let absent = verdict(&f, Verdict::Clean);
    f.reviewers.insert("absent".into(), absent);
    assert_eq!(step(&f, &m), Step::Merge);
    // A stale one does not, and nobody can look now.
    f.reviewers
        .get_mut("absent")
        .unwrap()
        .review
        .as_mut()
        .unwrap()
        .revision
        .head = head(2);
    assert_eq!(next(&f, &m, 4000).0.stage, Stage::Reviewer);
    // Nor does a current clean verdict with lost findings still owed.
    let absent = verdict(&f, Verdict::Clean);
    f.reviewers.insert("absent".into(), absent);
    let mut m = m;
    m.lost_on.insert(
        "absent".into(),
        BTreeMap::from([(f.revision.base.clone(), head(3))]),
    );
    assert_eq!(next(&f, &m, 4000).0.stage, Stage::Reviewer);
}

#[test]
fn r2_land_is_released_by_the_owner_for_its_revision_only() {
    // handle.py: "the owner has a label to take off ... their answer".
    let (_, mut f, m) = setup();
    f.classified.class = conary_core::Class::R2;
    f.judgment = Some(Judged {
        revision: f.revision.clone(),
        verdict: Judgment::Land,
    });
    // Asking the owner is not their answer, however often it is asked.
    let (n, m) = next(&f, &m, 4000);
    assert_eq!(n.step, Step::ListOwner);
    assert_eq!(m.owner_saw, None);
    assert_eq!(step(&f, &m), Step::ListOwner);
    // The label seen on this revision, then taken off, is.
    f.labels.insert("needs-peter".into());
    let (n, m) = next(&f, &m, 4000);
    assert_eq!(n.step, Step::ListOwner);
    assert_eq!(m.owner_saw.as_ref(), Some(&f.revision));
    f.labels.clear();
    assert_eq!(step(&f, &m), Step::Merge);
    // Not for another head, base or description, even with a fresh Land.
    let changes: [fn(&mut Revision); 3] = [
        |r| r.head = head(4),
        |r| r.base = "release".into(),
        |r| r.description.push('!'),
    ];
    for change in changes {
        let mut g = f.clone();
        change(&mut g.revision);
        g.reviewers
            .get_mut("reviewer")
            .unwrap()
            .review
            .as_mut()
            .unwrap()
            .revision = g.revision.clone();
        g.judgment = Some(Judged {
            revision: g.revision.clone(),
            verdict: Judgment::Land,
        });
        assert_eq!(step(&g, &m), Step::ListOwner);
    }
}

#[test]
fn label_without_an_owed_decision_is_not_approval() {
    // handle.py: "A label taken off before the judge said anything isn't an
    // answer to it"; neither is one on a judgment that needs no person.
    let (_, mut f, m) = setup();
    f.classified.class = conary_core::Class::R2;
    f.labels.insert("needs-peter".into());
    let (_, seen) = next(&f, &m, 4000);
    assert_eq!(seen.owner_saw, None);
    f.classified.class = conary_core::Class::R0;
    f.judgment = Some(Judged {
        revision: f.revision.clone(),
        verdict: Judgment::Land,
    });
    let (_, seen) = next(&f, &m, 4000);
    assert_eq!(seen.owner_saw, None);
}
