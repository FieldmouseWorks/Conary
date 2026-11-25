//! Stage label names come from the flow configuration (`settings.stage_labels`),
//! with defaults that name no person (#492).
use super::flow_pass_case::{Case, REPO};
use crate::api::Source;
use crate::flow::{self, Stage};
use serde_json::json;

fn labels_on(c: &Case) -> Vec<String> {
    c.hub
        .pull(REPO, 3)
        .unwrap()
        .labels
        .into_iter()
        .map(|l| l.name)
        .collect()
}

#[test]
fn configured_owner_stage_label_is_put_on_and_taken_off() {
    let mut c = Case::new();
    c.config.settings.stage_labels.owner = "waiting: Alice".into();
    c.config.settings.stage_labels.review = "stage/review".into();
    // An outside contributor's PR enters Stage::Owner (untrusted door).
    c.hub
        .edit_pull(REPO, 3, |p| p["user"]["login"] = json!("outsider"));

    let now = flow::jobs::now();
    let report = c.pass(now, false);
    assert_eq!(report[0].error, None);
    assert_eq!(report[0].next.as_ref().unwrap().stage, Stage::Owner);

    let on = labels_on(&c);
    assert!(
        on.iter().any(|l| l == "waiting: Alice"),
        "configured owner stage label must be put on: {on:?}"
    );
    assert!(
        !on.iter()
            .any(|l| l == "waiting: Peter" || l == "waiting: owner"),
        "hard-coded or default owner stage label must not appear: {on:?}"
    );

    // Moving to another stage takes the configured owner label off and puts
    // the new stage's configured label on.
    c.config.trusted_authors.push("outsider".into());
    // Clear the owner_label so the PR moves on to Stage::Review.
    c.hub.label(REPO, 3, &["waiting: Alice"]);
    let report = c.pass(now + 1, false);
    assert_eq!(report[0].error, None);
    assert_eq!(report[0].next.as_ref().unwrap().stage, Stage::Review);

    let after_move = labels_on(&c);
    assert!(
        !after_move.iter().any(|l| l == "waiting: Alice"),
        "configured owner stage label must be taken off on stage change: {after_move:?}"
    );
    assert!(
        after_move.iter().any(|l| l == "stage/review"),
        "configured review stage label must replace it: {after_move:?}"
    );

    // Moving back to Stage::Owner puts the configured owner label back on,
    // and moving to Stage::None (hold) takes it off without leaving any stage label.
    c.config.trusted_authors.retain(|a| a != "outsider");
    let report = c.pass(now + 2, false);
    assert_eq!(report[0].next.as_ref().unwrap().stage, Stage::Owner);
    assert!(labels_on(&c).iter().any(|l| l == "waiting: Alice"));

    c.hub.label(REPO, 3, &["waiting: Alice", "hold"]);
    let report = c.pass(now + 3, false);
    assert_eq!(report[0].error, None);
    assert_eq!(report[0].next.as_ref().unwrap().stage, Stage::None);
    let after_hold = labels_on(&c);
    assert!(
        !after_hold.iter().any(|l| l == "waiting: Alice"),
        "configured owner stage label must be taken off on Stage::None: {after_hold:?}"
    );
    assert!(
        after_hold.iter().any(|l| l == "hold"),
        "unrelated hold label must remain: {after_hold:?}"
    );
}

#[test]
fn default_owner_stage_label_names_no_person() {
    let c = Case::new();
    c.hub
        .edit_pull(REPO, 3, |p| p["user"]["login"] = json!("outsider"));

    let report = c.pass(flow::jobs::now(), false);
    assert_eq!(report[0].error, None);
    assert_eq!(report[0].next.as_ref().unwrap().stage, Stage::Owner);

    let on = labels_on(&c);
    assert!(
        on.iter().any(|l| l == "waiting: owner"),
        "default owner stage label must be generic: {on:?}"
    );
    assert!(
        !on.iter().any(|l| l.contains("Peter")),
        "default stage labels must name no person: {on:?}"
    );
}
