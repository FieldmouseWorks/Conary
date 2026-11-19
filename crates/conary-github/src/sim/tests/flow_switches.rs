//! Every switch in effect shows, with what it holds: each PR's decision
//! names the switches that stop it, each with the step it keeps off and the
//! workers it's stopping, whichever of them decided the PR's step. The
//! assertions read the decisions as `cn flow pass` prints them.
use super::flow_pass_case::{Case, REPO};
use super::head;
use crate::api::Source;
use crate::flow::*;
use serde_json::{Value, json};

fn repo() -> conary_core::op::Repo {
    REPO.parse().unwrap()
}
/// PR 3 and PR 5, both trusted and green, both waiting on their review.
fn two() -> Case {
    let mut c = Case::new();
    c.hub.pr(REPO, 5, &head(5));
    c.hub.edit_pull(REPO, 5, |p| {
        p["user"]["login"] = json!("owner");
        p["created_at"] = json!("2099-01-01T00:00:00Z");
    });
    let mut snapshot = c.hub.snapshot(REPO, 3).unwrap();
    snapshot.revision = Revision::from(&c.hub.pull(REPO, 5).unwrap());
    c.hub.flow_snapshot(REPO, 5, snapshot);
    c.good_ci(&head(5));
    c.config.plugins.review = None;
    c
}
fn printed(reports: &[Report]) -> Vec<Value> {
    reports
        .iter()
        .map(|r| serde_json::to_value(r).unwrap())
        .collect()
}
fn review() -> Value {
    json!({"QueueReview": "reviewer"})
}

/// A paused repository with a held PR and no review plugin: each PR's
/// decision names every switch that stops it, though the pause alone
/// decided its step. Lifting the pause and the hold leaves the plugin.
#[test]
fn a_pause_a_hold_and_an_omitted_plugin_each_name_what_they_stop() {
    let c = two();
    let now = crate::flow::jobs::now();
    // Without a pause or a hold, the omitted plugin is what stops both.
    for report in printed(&c.pass(now, false)) {
        assert_eq!(report["description"], "No plugin configured for review");
        assert_eq!(
            report["switches"],
            json!([{"switch": "no_plugin", "kind": "review", "step": review(), "workers": []}]),
            "{report}"
        );
    }

    c.hub.label(REPO, 3, &["hold"]);
    let paused = Pause {
        scope: Scope::Repository,
        reason: "moving the machine".into(),
        at: 7,
    };
    crate::flow::pause::pause(c.dir.path(), &repo(), &paused).unwrap();
    let reports = printed(&c.pass(now + 1, false));
    let pause = json!({"switch": "pause", "reason": "moving the machine", "at": 7,
        "step": review(), "workers": []});
    let hold = json!({"switch": "hold", "label": "hold", "step": review(), "workers": []});
    let plugin = json!({"switch": "no_plugin", "kind": "review", "step": review(), "workers": []});
    assert_eq!(reports[0]["pr"], 3);
    assert_eq!(reports[0]["description"], "Paused: moving the machine");
    assert_eq!(
        reports[0]["switches"],
        json!([pause, hold, plugin]),
        "{}",
        reports[0]
    );
    assert_eq!(reports[1]["pr"], 5);
    assert_eq!(
        reports[1]["switches"],
        json!([pause, plugin]),
        "{}",
        reports[1]
    );

    crate::flow::pause::resume(c.dir.path(), &repo()).unwrap();
    c.hub.label(REPO, 3, &[]);
    for report in printed(&c.pass(now + 2, false)) {
        assert_eq!(report["switches"], json!([plugin]), "{report}");
    }
}
