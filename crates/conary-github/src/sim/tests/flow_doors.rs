//! The owner is asked only for a named one-way door, and once per decision
//! (`owner-only-for-doors`, `one-question-per-decision`).
use super::flow_case::Case;
use super::head;
use crate::flow::*;
use crate::sim::REMI;
use serde_json::{Value, json};

fn request(n: &Next) -> Value {
    serde_json::to_value(n).unwrap()["owner"].clone()
}

#[test]
fn too_much_to_read_is_the_flows_own_wait_not_the_owners() {
    let c = Case::new();
    c.hub.edit_flow(REMI, 3, |s| s.complete = false);
    let n = c.view(4000);
    assert!(!matches!(n.step, Step::ListOwner { .. }), "{n:?}");
    assert_eq!(n.stage, Stage::None);
    assert_eq!(request(&n), Value::Null);
    assert_eq!(
        n.overdue_at,
        Some(4000 + c.settings.stall_after.at(Level::Medium))
    );
}

#[test]
fn an_r2_request_joins_across_base_merge_heads() {
    let mut c = Case::new();
    c.secret();
    c.judge("pass");
    let first = c.scope();
    let n = c.pass(4000);
    assert!(matches!(n.step, Step::ListOwner { .. }), "{n:?}");
    let asked = request(&n);
    let marker = owner_approval(&REMI.parse().unwrap(), 3, &first);
    assert_eq!(asked["marker"], json!(marker));
    assert_eq!(asked["key"]["door"], json!("r2"));
    for (i, at) in [(4, 5000), (5, 6000)] {
        c.push(&head(i));
        c.secret();
        c.review("reviewer", Verdict::Clean, at);
        c.judge("pass");
        let n = c.pass(at + 1);
        assert!(matches!(n.step, Step::ListOwner { .. }), "{n:?}");
        let joined = request(&n);
        assert_eq!(joined["key"], asked["key"]);
        assert_eq!(joined["since"], asked["since"]);
        let marker = owner_approval(&REMI.parse().unwrap(), 3, &c.scope());
        assert_eq!(joined["marker"], json!(marker));
        assert_eq!(joined["prs"], json!([3]));
    }
    let open = serde_json::to_value(&c.memory).unwrap()["owner"].clone();
    assert_eq!(open.as_array().map(Vec::len), Some(1), "{open}");
    // Joining never widens an approval: one on the first head approves
    // no other.
    c.settings.people.insert("maintainer".into());
    c.hub.person_review(
        REMI,
        3,
        "maintainer",
        "User",
        &first.head,
        &owner_approval(&REMI.parse().unwrap(), 3, &first),
        "APPROVED",
    );
    assert!(matches!(c.view(7000).step, Step::ListOwner { .. }));
    c.approve();
    assert_eq!(c.view(7000).step, Step::Merge);
}

#[test]
fn a_new_intent_is_a_new_decision() {
    let mut c = Case::new();
    c.secret();
    c.judge("pass");
    let asked = request(&c.pass(4000));
    c.description("another intent", 4500);
    c.review("reviewer", Verdict::Clean, 4600);
    c.judge("pass");
    let n = c.pass(5000);
    assert_ne!(request(&n)["key"], asked["key"]);
    let open = serde_json::to_value(&c.memory).unwrap()["owner"].clone();
    assert_eq!(open.as_array().map(Vec::len), Some(1), "{open}");
}

#[test]
fn two_lost_judges_stall_in_status_without_the_owner() {
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
    let n = c.pass(4001);
    assert!(!matches!(n.step, Step::ListOwner { .. }), "{n:?}");
    assert!(matches!(n.step, Step::Wait { .. }), "{n:?}");
    assert_eq!(n.overdue_at, Some(4001), "overdue at once");
    assert_eq!(n.owner_notice, None);
}

#[test]
fn a_held_major_names_no_door_so_it_waits_unlisted() {
    let mut c = Case::new();
    c.hub
        .edit_pull(REMI, 3, |p| p["head"]["ref"] = json!("renovate/pkg"));
    c.label(&["major"]);
    c.sweep("held");
    let n = c.view(4000);
    assert!(!matches!(n.step, Step::ListOwner { .. }), "{n:?}");
    assert!(n.overdue_at.is_some(), "{n:?}");
}

#[test]
fn a_hold_past_its_deadline_is_a_stall_never_an_owner_listing() {
    let mut c = Case::new();
    c.secret();
    c.label(&["hold"]);
    let n = c.pass(4000);
    let late = 4000 + c.settings.hold_stall_after + 1;
    let n2 = c.pass(late);
    for n in [n, n2] {
        assert!(!matches!(n.step, Step::ListOwner { .. }), "{n:?}");
        assert_eq!(n.owner_notice, None);
        assert_eq!(n.overdue_at, Some(4000 + c.settings.hold_stall_after));
    }
}

#[test]
fn the_flows_own_label_is_no_door() {
    let mut c = Case::new();
    c.secret();
    c.judge("pass");
    assert_eq!(c.pass(4000).step, Step::ListOwner(Door::R2));
    // The pass put the label on for the request.
    c.label(&["needs-peter"]);
    c.memory.labelled = true;
    c.memory.owner[0].listed = true;
    let n = c.pass(4001);
    assert_eq!(n.step, Step::ListOwner(Door::R2));
    assert_eq!(request(&n)["listed"], json!(true));
    // Once a pass reads it off, a person's label is their door.
    c.label(&[]);
    c.pass(4002);
    assert!(!c.memory.labelled);
    c.label(&["needs-peter"]);
    assert_eq!(c.pass(4003).step, Step::ListOwner(Door::Label));
}

/// The owner label as the steward puts it on, by its App.
fn steward_labels(c: &Case) {
    c.hub
        .label_as(REMI, 3, &["needs-peter"], (crate::sim::STEWARD, "Bot"));
}

#[test]
fn a_label_an_app_put_on_lists_nothing_of_its_own() {
    // The steward listed the PR, with no mark of the flow's: its label is
    // no door, and the flow makes its own call.
    let c = Case::new();
    let own = c.view(4000);
    steward_labels(&c);
    let n = c.view(4000);
    assert_eq!(n.step, own.step, "{n:?}");
    assert_eq!(n.description, own.description);
    // Taken off and put back by a person, it's their door.
    c.label(&[]);
    c.label(&["needs-peter"]);
    assert_eq!(c.view(4001).step, Step::ListOwner(Door::Label));
}

#[test]
fn two_lost_jobs_the_steward_listed_stay_the_flows_own_stall() {
    let mut c = Case::new();
    steward_labels(&c);
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
    let n = c.pass(4001);
    assert!(n.description.starts_with("Two jobs ended"), "{n:?}");
    assert_eq!(n.overdue_at, Some(4001));
}

#[test]
fn who_labelled_it_is_read_only_while_on_and_not_the_flows_own() {
    let mut c = Case::new();
    c.view(4000);
    assert_eq!(c.hub.count(crate::sim::Call::Timeline), 0);
    c.label(&["needs-peter"]);
    c.memory.labelled = true;
    c.view(4001);
    assert_eq!(c.hub.count(crate::sim::Call::Timeline), 0);
    c.memory.labelled = false;
    c.view(4002);
    assert_eq!(c.hub.count(crate::sim::Call::Timeline), 1);
}

#[test]
fn a_label_whose_timeline_names_no_one_waits_overdue() {
    // GitHub answers, with no `labeled` event for it: who put it on can't
    // be told, and the wait shows in the status like any other.
    let c = Case::new();
    c.hub
        .edit_pull(REMI, 3, |p| p["labels"] = json!([{"name": "needs-peter"}]));
    let n = c.view(4000);
    assert!(
        n.description.contains("who put it on can't be told"),
        "{n:?}"
    );
    assert_eq!(n.stage, Stage::None);
    assert_eq!(
        n.overdue_at,
        Some(4000 + c.settings.stall_after.at(Level::Medium))
    );
}

#[test]
fn a_label_no_one_can_be_told_put_on_waits_named() {
    // The read fails: the label is neither listed as a person's door nor
    // gone past, and the read is named.
    let c = Case::new();
    c.label(&["needs-peter"]);
    c.hub.fail(crate::sim::Call::Timeline);
    let n = c.view(4000);
    assert!(
        n.description.contains("who put it on can't be told"),
        "{n:?}"
    );
    assert!(matches!(n.step, Step::Wait { .. }), "{n:?}");
    let met = c.facts().reads.met;
    assert!(
        met.iter().any(|d| d.what
            == Degraded::Read {
                read: Reading::OwnerLabel
            }),
        "{met:?}"
    );
    c.hub.heal(crate::sim::Call::Timeline);
    assert_eq!(c.view(4001).step, Step::ListOwner(Door::Label));
}

mod pass {
    use super::super::flow_pass_case::{Case, REPO};
    use crate::api::Source;
    use crate::flow::*;
    use crate::sim::Call;
    use serde_json::json;

    fn decided(reports: &[Report]) -> &Next {
        reports
            .iter()
            .find(|r| r.pr == 3)
            .unwrap()
            .next
            .as_ref()
            .unwrap()
    }
    fn label(c: &Case) -> bool {
        let wanted = c.config.settings.owner_label.as_deref();
        c.hub
            .pull(REPO, 3)
            .unwrap()
            .labels
            .iter()
            .any(|l| Some(&*l.name) == wanted)
    }

    #[test]
    fn a_steward_label_a_person_puts_back_before_the_merge_withdraws_it() {
        // Decided to merge past the steward's label; before the merge's
        // scope is checked, a person takes it off and puts it back. The
        // labels read the same, but the door is now a person's.
        let c = Case::new();
        c.clean_review();
        c.hub
            .label_as(REPO, 3, &["needs-peter"], (crate::sim::STEWARD, "Bot"));
        // The decision's read, then the class's: the scope's is third.
        c.hub.race(Call::Timeline, |h| {
            h.race(Call::Timeline, |h| {
                h.race(Call::Timeline, |h| {
                    h.label(REPO, 3, &[]);
                    h.label(REPO, 3, &["needs-peter"]);
                })
            })
        });
        let now = jobs::now() + 10_000;
        let report = c.pass(now, false).into_iter().find(|r| r.pr == 3).unwrap();
        assert_eq!(report.next.as_ref().unwrap().step, Step::Merge);
        assert!(
            report
                .error
                .as_deref()
                .is_some_and(|e| e.contains("scope changed")),
            "{report:?}"
        );
        assert_eq!(c.hub.count(Call::Merge), 0);
    }

    #[test]
    fn a_judges_door_under_the_stewards_label_is_listed_once() {
        // The steward listed the PR before; the flow's judge sends it to
        // the owner. The judge's door is the one listed, once, and the
        // steward's label is no door of its own on either pass.
        let c = Case::new();
        c.clean_review();
        c.judgment(1, Judgment::Owner, true);
        c.hub
            .label_as(REPO, 3, &["needs-peter"], (crate::sim::STEWARD, "Bot"));
        let now = jobs::now();
        for at in [now, now + 1] {
            let report = c.pass(at, false).into_iter().find(|r| r.pr == 3).unwrap();
            assert_eq!(report.error, None, "{report:?}");
            assert_eq!(
                report.next.unwrap().step,
                Step::ListOwner(Door::Judge),
                "{at}"
            );
        }
        assert_eq!(c.hub.count(Call::FlowOwner), 1);
    }

    #[test]
    fn the_label_a_judges_listing_put_on_is_no_door_of_its_own() {
        // The judge's door is decided after the label's: the label the
        // flow put on for it must not read as a person's next pass.
        let c = Case::new();
        c.clean_review();
        c.judgment(1, Judgment::Owner, true);
        let now = jobs::now();
        for at in [now, now + 1] {
            let n = decided(&c.pass(at, false)).clone();
            assert_eq!(n.step, Step::ListOwner(Door::Judge), "{n:?}");
        }
        assert!(label(&c));
        assert_eq!(c.hub.count(Call::FlowOwner), 1);
    }

    #[test]
    fn an_outside_contribution_is_listed_once_across_heads() {
        let c = Case::new();
        c.hub
            .edit_pull(REPO, 3, |p| p["user"]["login"] = json!("outsider"));
        let now = jobs::now();
        let first = decided(&c.pass(now, false)).clone();
        assert_eq!(first.step, Step::ListOwner(Door::Outside));
        assert_eq!(c.hub.count(Call::FlowOwner), 1);
        assert!(label(&c));
        assert!(c.record().memory.labelled);
        // A new head on the same intent joins the request: the flow's own
        // label doesn't turn it into the label's door, and nothing is
        // listed again.
        let sha = crate::sim::tree_of(&["outsider's second head"]);
        c.hub.push(REPO, 3, &sha);
        c.hub.refresh_flow(REPO, 3);
        let second = decided(&c.pass(now + 1, false)).clone();
        assert_eq!(second.step, Step::ListOwner(Door::Outside));
        assert_eq!(second.owner.as_ref().unwrap().key, first.owner.unwrap().key);
        assert_eq!(c.hub.count(Call::FlowOwner), 1);
        // A person takes the label off: the decision is still the one asked.
        c.hub.label(REPO, 3, &[]);
        let third = decided(&c.pass(now + 2, false)).clone();
        assert_eq!(third.step, Step::ListOwner(Door::Outside));
        assert_eq!(c.hub.count(Call::FlowOwner), 1);
        assert!(!label(&c));
    }
}

#[test]
fn a_judges_request_carries_its_reason_outside_its_key() {
    let mut c = Case::new();
    c.judge("owner");
    c.judge_reason = Some("it rewrites who may land".into());
    let n = c.pass(4000);
    assert_eq!(n.step, Step::ListOwner(Door::Judge));
    let asked = request(&n);
    assert_eq!(asked["reason"], json!("it rewrites who may land"));
    assert_eq!(asked["key"]["door"], json!("judge"));
    c.judge_reason = Some("reworded".into());
    let joined = request(&c.pass(4001));
    assert_eq!(joined["key"], asked["key"]);
    assert_eq!(joined["since"], json!(4000));
    assert_eq!(joined["reason"], json!("reworded"));
}

#[test]
fn two_lost_jobs_stay_overdue_on_a_pr_open_over_a_day() {
    let mut c = Case::new();
    c.hub
        .edit_pull(REMI, 3, |p| p["created_at"] = json!("1970-01-01T00:00:00Z"));
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
    c.pass(100_000);
    let n = c.pass(100_001);
    assert!(n.description.starts_with("Two jobs ended"), "{n:?}");
    assert_eq!(n.owner_notice, None);
    assert_eq!(n.overdue_at, Some(100_001));
}

#[test]
fn two_lost_fixes_of_a_timed_out_run_go_to_its_recovery() {
    // A timed-out run is red, and two fix jobs ended without their answer:
    // no third starts, and the run is asked for again by its kind, once.
    let mut c = Case::new();
    c.hub.put_checks(
        &head(3),
        json!([{"id":701,"name":"check","status":"completed","conclusion":"timed_out",
            "check_suite":{"id":7}}]),
    );
    c.hub.edit_flow(REMI, 3, |s| {
        s.suites.insert(
            7,
            Suite {
                status: "completed".into(),
                conclusion: Some("timed_out".into()),
                app: "github-actions".into(),
                workflow: Some(Workflow {
                    id: "ci".into(),
                    name: "ci".into(),
                    path: Some(".github/workflows/ci.yml".into()),
                    run: Some(9007),
                }),
            },
        );
    });
    c.jobs.insert(
        Kind::Red,
        Job {
            id: "fix".into(),
            revision: c.scope(),
            state: JobState::Done,
            started: 0,
            repeated: true,
        },
    );
    c.pass(4000);
    let n = c.pass(4001);
    assert_eq!(n.stage, Stage::Fix);
    assert_eq!(n.step, Step::Recover(Recovery::Rerun(9007)), "{n:?}");
    assert_eq!(n.overdue_at, Some(4001));
}
