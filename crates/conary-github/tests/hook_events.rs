//! External-caller checks for the exported webhook event list (`events::hook_events`).
use conary_github::events::{hook_events, kicks};
use serde_json::Value;
use std::collections::BTreeSet;

#[test]
fn hook_events_matches_the_table_and_has_no_duplicates() {
    let listed: Vec<&'static str> = hook_events().collect();
    let exported: BTreeSet<&str> = listed.iter().copied().collect();
    assert_eq!(
        listed.len(),
        exported.len(),
        "hook_events must not contain duplicate event names"
    );

    let repos: BTreeSet<String> = ["o/r".into()].into_iter().collect();
    let fixtures: Value =
        serde_json::from_str(include_str!("../src/sim/tests/fixtures/webhooks.json")).unwrap();
    let mut kicking = BTreeSet::new();
    for row in fixtures.as_array().unwrap() {
        let event = row["event"].as_str().unwrap();
        if !kicks(event, &row["payload"], &repos).is_empty() {
            kicking.insert(event.to_owned());
        }
    }

    let exported_owned: BTreeSet<String> = exported.iter().map(|s| (*s).to_owned()).collect();
    assert_eq!(
        exported_owned, kicking,
        "hook_events must match every event in the receiver's event-to-kick table"
    );
    assert!(!exported.contains("label"));
    assert!(!exported.contains("ping"));
}
