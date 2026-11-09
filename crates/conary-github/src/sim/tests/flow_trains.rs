use super::flow_case::Case;
use crate::flow::*;
use crate::sim::REMI;
fn train() -> Case {
    let mut c = Case::new();
    c.settings.train = true;
    c
}
fn seated(c: &mut Case) {
    c.train.seat = Some(Seat {
        head: c.scope().head,
        ahead: 0,
        candidate: None,
    });
}
#[test]
fn a_joined_head_waits_in_its_actual_candidate_without_joining_again() {
    // trains.py:87: own gating/landing candidate, a candidate ahead, and the idle queue.
    for (contains, landing, ahead, phrase) in [
        (true, false, 0, "gating in candidate ABCDEFGH with #7"),
        (true, true, 0, "landing in candidate ABCDEFGH"),
        (false, false, 1, "behind candidate ABCDEFGH, 1 ahead"),
        (false, false, 2, "behind candidate ABCDEFGH, 2 ahead"),
    ] {
        let mut c = train();
        seated(&mut c);
        let s = c.train.seat.as_mut().unwrap();
        s.ahead = ahead;
        s.candidate = Some(Candidate {
            id: "ABCDEFGHJKMNPQRSTVWXYZ0123".into(),
            contains,
            landing,
            peers: if landing { vec![] } else { vec![7] },
        });
        let n = c.pass(700);
        assert_eq!(n.step, Step::Wait { until: None });
        assert_eq!(n.stage, Stage::Train);
        assert!(n.description.contains(phrase));
        assert_eq!(n.wait_on, Some(WaitOn::Train));
        c.hub
            .edit_flow(REMI, 3, |s| s.merge_state = "UNKNOWN".into());
        assert_eq!(c.view(800).step, Step::Wait { until: None });
    }
    let mut c = train();
    seated(&mut c);
    c.train.seat.as_mut().unwrap().ahead = 2;
    assert!(c.view(800).description.contains("2 ahead"));
}
#[test]
fn a_new_head_or_a_missing_ledger_seat_joins_again() {
    // trains.py:110 / 114: disappearance from the ledger and a pushed head both rejoin.
    let mut c = train();
    seated(&mut c);
    c.push("new-train-head");
    c.review("reviewer", Verdict::Clean, 600);
    assert_eq!(c.view(700).step, Step::JoinTrain);
    c.train.seat = None;
    assert_eq!(c.view(800).step, Step::JoinTrain);
}
#[test]
fn an_unready_pr_leaves_the_train_before_more_work() {
    // trains.py:174-191: findings, an unreviewed push, a draft or a hold all withdraw the PR.
    for scenario in 0..4 {
        let mut c = train();
        seated(&mut c);
        match scenario {
            0 => c.threads(1),
            1 => c.push("unreviewed-head"),
            2 => c.hub.edit_flow(REMI, 3, |s| s.draft = true),
            _ => c.label(&["hold"]),
        }
        assert_eq!(c.view(800).step, Step::LeaveTrain);
        c.train.seat = None;
        assert_ne!(c.view(900).step, Step::LeaveTrain);
    }
}
#[test]
fn retargeting_off_the_default_branch_leaves_then_merges_normally() {
    // trains.py: release branches land outside the train, even if previously seated.
    let mut c = train();
    seated(&mut c);
    c.hub.set_branch(REMI, "release", super::TRUNK);
    c.hub.retarget(REMI, 3, "release");
    c.hub.refresh_flow(REMI, 3);
    c.review("reviewer", Verdict::Clean, 700);
    assert_eq!(c.view(800).step, Step::LeaveTrain);
    c.train.seat = None;
    assert_eq!(c.view(900).step, Step::Merge);
}
#[test]
fn held_train_reasons_are_head_scoped_and_do_not_prevent_rejoining() {
    // trains.py:137 / 208 / 223: raised, conflict, empty, red, refused and negative evidence holds.
    for reason in [
        "class raised since it was settled",
        "conflict after PRs ahead",
        "empty after PRs ahead",
        "red alone until a new head",
        "GitHub refused its merge",
        "negative has not finished on this head",
    ] {
        let mut c = train();
        c.train.held = Some((c.scope().head, reason.into()));
        let n = c.view(800);
        assert_eq!(n.step, Step::JoinTrain);
        assert!(n.description.contains(reason));
        assert_eq!(n.stage, Stage::Train);
        c.push("new-held-head");
        c.review("reviewer", Verdict::Clean, 900);
        assert!(!c.view(1300).description.contains(reason));
    }
}
#[test]
fn a_queued_github_merge_is_taken_off_before_joining_or_waiting_on_hold() {
    // trains.py:260 / passes.py:29: queued merges must not bypass the train or an unready status.
    let c = train();
    c.hub.edit_pull(REMI, 3, |p| {
        p["auto_merge"] = serde_json::json!({"enabled_by":{"login":"owner"}})
    });
    assert_eq!(c.view(800).step, Step::DisableAutoMerge);
    c.label(&["hold"]);
    assert_eq!(c.view(900).step, Step::DisableAutoMerge);
    c.hub
        .edit_pull(REMI, 3, |p| p["auto_merge"] = serde_json::Value::Null);
    assert!(matches!(c.view(1000).step, Step::Wait { .. }));
}

#[test]
fn train_ci_can_cover_missing_pr_ci_but_never_unknown_authorship() {
    // trains.py:275,289,295: no CI and dependency bot use train, unread provenance never does.
    let mut c = train();
    c.no_ci();
    assert_eq!(c.view(800).step, Step::JoinTrain);
    c.hub.edit_pull(REMI, 3, |p| {
        p["head"]["ref"] = serde_json::json!("renovate/foo");
        p["title"] = serde_json::json!("Update foo to v1.5");
    });
    c.hub.refresh_flow(REMI, 3);
    c.settings.renovate_email = Some("bot@example.org".into());
    c.hub.commit_details(
        &c.scope().head,
        Some("deps: bump"),
        Some("bot@example.org"),
        Some("bot@example.org"),
    );
    assert_eq!(c.view(800).step, Step::JoinTrain);
    c.hub.fail(crate::sim::Call::Commit);
    assert_ne!(c.view(800).step, Step::JoinTrain);
}
#[test]
fn train_admission_keeps_class_raises_and_requires_a_person_for_r2() {
    // trains.py:80,119,131,440,457: raised class persists and gates admission.
    // R2 deliberately requires the person specified by #263, even after a judge's land.
    for labels in [&["review"][..], &["r2"][..], &["Review", "r2"][..]] {
        let mut c = train();
        c.label(labels);
        if labels.contains(&"r2") {
            c.judge("land");
            assert_eq!(c.pass(800).step, Step::ListOwner);
            c.label(&["needs-peter", "r2"]);
            c.pass(810);
            c.label(&[]);
            assert_eq!(c.pass(819).step, Step::ListOwner);
            c.approve();
            assert_eq!(c.pass(820).step, Step::JoinTrain);
        } else {
            assert_eq!(c.pass(800).step, Step::JoinTrain);
            c.label(&[]);
            c.push("raised-train-head");
            c.review("reviewer", Verdict::Clean, 900);
            assert_eq!(c.pass(1300).step, Step::JoinTrain);
        }
        assert!(!c.memory.raised.is_empty());
    }
}
