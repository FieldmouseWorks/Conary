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
    let label = c.config.settings.stage_labels.label(stage).unwrap();
    c.hub.label(REPO, 3, &[label, "unrelated"]);
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
    let (lines, tally) = beside(&c.hub, dry(&c), &authors(), &c.config.settings.stage_labels);
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
    let (lines, tally) = beside(&c.hub, dry(&c), &authors(), &c.config.settings.stage_labels);
    assert!(!lines[0].agree);
    assert_eq!(lines[0].flow.as_ref().unwrap().stage, stage);
    assert_eq!(lines[0].steward.as_ref().unwrap().stages, [other]);
    assert_eq!((tally.agree, tally.disagree), (0, 1));
    for call in [
        Call::FlowStatus,
        Call::FlowStage,
        Call::FlowComment,
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
    let (lines, _) = beside(&c.hub, dry(&c), &authors(), &c.config.settings.stage_labels);
    assert_eq!(lines[0].steward.as_ref().unwrap().state, None);
    // With no author named, anyone's newest counts.
    let (lines, _) = beside(
        &c.hub,
        dry(&c),
        &BTreeSet::new(),
        &c.config.settings.stage_labels,
    );
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
    let (lines, tally) = beside(&c.hub, reports, &authors(), &c.config.settings.stage_labels);
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
    let labels = StageLabels::default();
    for stage in Stage::LABELLED {
        assert_eq!(labels.stage(labels.label(stage).unwrap()), Some(stage));
    }
    assert_eq!(labels.stage("waiting: nothing"), None);
}

#[test]
fn compare_reads_configured_stage_labels() {
    let mut c = Case::new();
    c.config.settings.stage_labels.owner = "waiting: Alice".into();
    c.hub
        .edit_pull(REPO, 3, |p| p["user"]["login"] = json!("outsider"));
    let reports = dry(&c);
    assert_eq!(reports[0].next.as_ref().unwrap().stage, Stage::Owner);

    // The configured owner stage label agrees; the default or another machine's does not.
    c.hub.label(REPO, 3, &["waiting: Alice"]);
    let (lines, _) = beside(&c.hub, dry(&c), &authors(), &c.config.settings.stage_labels);
    assert!(lines[0].agree, "{lines:?}");
    assert_eq!(lines[0].steward.as_ref().unwrap().stages, [Stage::Owner]);

    for other in ["waiting: owner", "waiting: Peter"] {
        c.hub.label(REPO, 3, &[other]);
        let (lines, _) = beside(&c.hub, dry(&c), &authors(), &c.config.settings.stage_labels);
        assert!(!lines[0].agree, "{other}: {lines:?}");
        assert!(lines[0].steward.as_ref().unwrap().stages.is_empty());
    }
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
        let (lines, tally) = beside(&c.hub, reports, &authors(), &c.config.settings.stage_labels);
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
    let (lines, tally) = beside(&c.hub, reports, &authors(), &c.config.settings.stage_labels);
    assert_eq!(
        lines[0].error.as_deref(),
        Some("the flow's decision names no revision")
    );
    assert_eq!(tally.errors, 1);
}

/// A PR the flow's pause stops disagrees with a steward that has no pause,
/// and its line names the switch, so the shadow tells it apart from a
/// different reading of the PR.
#[test]
fn compare_names_the_switch_behind_a_disagreement() {
    let c = Case::new();
    let stage = dry(&c)[0].next.as_ref().unwrap().stage;
    steward_says(&c, stage, "pending", STEWARD);
    let paused = Pause {
        scope: Scope::Repository,
        reason: "moving the machine".into(),
        at: 7,
    };
    flow::pause::pause(c.dir.path(), &REPO.parse().unwrap(), &paused).unwrap();
    let (lines, tally) = beside(&c.hub, dry(&c), &authors(), &c.config.settings.stage_labels);
    assert!(!lines[0].agree);
    assert_eq!((tally.agree, tally.disagree), (0, 1));
    let line = serde_json::to_value(&lines[0]).unwrap();
    assert_eq!(line["flow"]["switches"][0]["switch"], "pause", "{line}");
    assert_eq!(line["flow"]["switches"][0]["reason"], "moving the machine");
}
