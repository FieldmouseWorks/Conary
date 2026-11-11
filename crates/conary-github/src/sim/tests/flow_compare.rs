//! A dry pass beside the steward's labels and status, for shadowing it.
use super::flow_pass_case::{Case, REPO};
use super::head;
use crate::flow::{self, *};
use crate::sim::Call;
use serde_json::json;
use std::collections::BTreeSet;

const STEWARD: &str = "steward[bot]";

fn authors() -> BTreeSet<String> {
    [STEWARD.to_string()].into()
}

fn steward_says(c: &Case, stage: Stage, state: &str, by: &str) {
    c.hub.label(REPO, 3, &[stage.label().unwrap(), "unrelated"]);
    c.hub.post_status(
        &head(3),
        json!({"context": SETTLED, "state": state, "description": "the steward's word",
               "creator": {"login": by, "type": "Bot"}}),
    );
}

fn dry(c: &Case) -> Vec<Report> {
    c.pass(flow::jobs::now(), true)
}

#[test]
fn compare_agrees_when_the_steward_labels_the_flows_stage() {
    let c = Case::new();
    let stage = dry(&c)[0].next.as_ref().unwrap().stage;
    steward_says(&c, stage, "pending", STEWARD);
    let (lines, tally) = beside(&c.hub, dry(&c), &authors());
    assert!(lines[0].agree, "{lines:?}");
    let said = lines[0].steward.as_ref().unwrap();
    assert_eq!(said.stages, [stage]);
    assert_eq!(said.state.as_deref(), Some("pending"));
    assert_eq!(said.description.as_deref(), Some("the steward's word"));
    assert_eq!(lines[0].head.as_deref(), Some(head(3).as_str()));
    assert_eq!(
        tally,
        Tally {
            repo: Some(REPO.parse().unwrap()),
            prs: 1,
            agree: 1,
            disagree: 0,
            errors: 0
        }
    );
}

#[test]
fn compare_disagrees_on_another_stage_and_reads_nothing_it_would_write() {
    let c = Case::new();
    let stage = dry(&c)[0].next.as_ref().unwrap().stage;
    let other = Stage::LABELLED.into_iter().find(|s| *s != stage).unwrap();
    steward_says(&c, other, "pending", STEWARD);
    let (lines, tally) = beside(&c.hub, dry(&c), &authors());
    assert!(!lines[0].agree);
    assert_eq!(lines[0].flow.as_ref().unwrap().stage, stage);
    assert_eq!(lines[0].steward.as_ref().unwrap().stages, [other]);
    assert_eq!((tally.agree, tally.disagree), (0, 1));
    for call in [
        Call::FlowStatus,
        Call::FlowStage,
        Call::FlowReview,
        Call::Land,
        Call::Mark,
        Call::FlowOwner,
    ] {
        assert_eq!(c.hub.count(call), 0, "{call:?}");
    }
}

#[test]
fn compare_counts_only_the_stewards_own_status() {
    let c = Case::new();
    let stage = dry(&c)[0].next.as_ref().unwrap().stage;
    steward_says(&c, stage, "success", "someone-else");
    let (lines, _) = beside(&c.hub, dry(&c), &authors());
    assert_eq!(lines[0].steward.as_ref().unwrap().state, None);
    // With no author named, anyone's newest counts.
    let (lines, _) = beside(&c.hub, dry(&c), &BTreeSet::new());
    assert_eq!(
        lines[0].steward.as_ref().unwrap().state.as_deref(),
        Some("success")
    );
}

#[test]
fn compare_lists_disagreements_then_errors_then_agreements() {
    let c = Case::new();
    let report = dry(&c).remove(0);
    let stage = report.next.as_ref().unwrap().stage;
    steward_says(&c, stage, "pending", STEWARD);
    let mut other = report.next.clone().unwrap();
    other.stage = Stage::LABELLED.into_iter().find(|s| *s != stage).unwrap();
    let revision = report.revision.clone();
    let reports = vec![
        report,
        Report {
            repo: REPO.parse().unwrap(),
            pr: 99,
            next: None,
            error: Some("unreadable".into()),
            owner_approval: None,
            revision: None,
        },
        Report {
            repo: REPO.parse().unwrap(),
            pr: 3,
            next: Some(other),
            error: None,
            owner_approval: None,
            revision,
        },
    ];
    let (lines, tally) = beside(&c.hub, reports, &authors());
    let order: Vec<_> = lines.iter().map(|l| (l.pr, l.agree)).collect();
    assert_eq!(order, [(3, false), (99, false), (3, true)]);
    assert_eq!(lines[1].error.as_deref(), Some("unreadable"));
    assert_eq!(
        (tally.prs, tally.agree, tally.disagree, tally.errors),
        (3, 1, 1, 1)
    );
}

#[test]
fn compare_maps_every_label_back_to_its_stage() {
    for stage in Stage::LABELLED {
        assert_eq!(Stage::from_label(stage.label().unwrap()), Some(stage));
    }
    assert_eq!(Stage::from_label("waiting: nothing"), None);
}

#[test]
fn compare_refuses_a_pr_that_moved_after_the_flow_decided() {
    let moves: [fn(&Case); 3] = [
        |c| c.hub.push(REPO, 3, &"a".repeat(40)),
        |c| c.hub.edit_pull(REPO, 3, |p| p["body"] = json!("rewritten")),
        |c| c.hub.edit_pull(REPO, 3, |p| p["title"] = json!("renamed")),
    ];
    for moved in moves {
        let c = Case::new();
        let reports = dry(&c);
        steward_says(
            &c,
            reports[0].next.as_ref().unwrap().stage,
            "pending",
            STEWARD,
        );
        moved(&c);
        let (lines, tally) = beside(&c.hub, reports, &authors());
        assert!(!lines[0].agree);
        assert!(
            lines[0].error.as_deref().unwrap().contains("changed"),
            "{lines:?}"
        );
        assert_eq!((tally.agree, tally.disagree, tally.errors), (0, 0, 1));
    }
}

#[test]
fn compare_refuses_a_decision_without_its_revision() {
    let c = Case::new();
    let mut reports = dry(&c);
    steward_says(
        &c,
        reports[0].next.as_ref().unwrap().stage,
        "pending",
        STEWARD,
    );
    reports[0].revision = None;
    let (lines, tally) = beside(&c.hub, reports, &authors());
    assert_eq!(
        lines[0].error.as_deref(),
        Some("the flow's decision names no revision")
    );
    assert_eq!(tally.errors, 1);
}
