//! A mark that doesn't parse is a degraded path too (`docs/flow.md`,
//! "Degraded paths"): a sealed verdict, the reviewer's summary or
//! Renovate's release-age wait on the PR, a worker's stop marker, or an
//! answer to another kind of work. Each is recorded once while it lasts,
//! counted as its check's failure, named in the PR's decision, and cleared
//! once it reads. Read as JSON, as `cn flow` and a ledger reader see them,
//! so these run on a tree that has no such path.
use super::flow_degraded::{decided, ops};
use super::flow_pass_case::{Case, REPO};
use super::flow_pass_slots::{prepared, two};
use super::head;
use crate::api::Source;
use crate::flow::*;
use serde_json::{Value, json};

/// The ops of kind `op` that name what `what` names.
fn naming(c: &Case, op: &str, what: &Value) -> usize {
    ops(c, op)
        .iter()
        .filter(|v| v.pointer("/op/body").or(v.pointer("/body")).unwrap()["what"] == *what)
        .count()
}
/// The PR's decision names `what`, and nothing else.
fn names_only(next: &Value, what: &Value) -> bool {
    next["degraded"].as_array().is_some_and(|d| {
        d.len() == 1 && {
            let mut named = d[0].clone();
            named.as_object_mut().unwrap().remove("why");
            named == *what
        }
    })
}
fn mark(name: &str) -> Value {
    json!({"path": "mark", "mark": name})
}

/// A Renovate PR whose only red check is its artifacts, Renovate's last
/// artifact comment saying `wait`.
fn renovate(c: &mut Case, wait: &str) {
    c.as_bot();
    c.config.settings.renovate_login = Some("renovate[bot]".into());
    c.hub.post_status(
        &head(3),
        json!({"context": "renovate/artifacts", "state": "failure"}),
    );
    c.hub.flow_comment(
        REPO,
        3,
        "renovate[bot]",
        &format!("Artifact update problem\n```\n{wait}\n```\n"),
    );
}
const RELEASE: &str = "lock-age: nothing in the refresh is old enough until 2099-01-02T00:00:00Z";

/// One mark of the family: how a test breaks it on the PR, how it's put
/// right, and whether the PR lands then.
struct Row {
    mark: &'static str,
    /// Leaves the mark broken, returning the index of the comment it's in.
    arm: fn(&mut Case) -> usize,
    /// What that comment says once it's put right.
    healed: &'static str,
    lands: bool,
}

/// The comments on PR 3 so far.
fn comments(c: &Case) -> usize {
    c.hub.comments(REPO, 3).unwrap().len()
}

/// Main went on past each as if it said nothing: a verdict it couldn't
/// read was passed over, and the reviewer's clean summary before a summary
/// it couldn't read stood, so the PR landed; and a release-age wait it
/// couldn't read sent a PR that only had to wait to the dependency sweep.
const ROWS: &[Row] = &[
    Row {
        mark: "verdict",
        arm: |c| {
            let at = comments(c);
            c.hub.flow_comment(
                REPO,
                3,
                "producer",
                r#"<!-- autoland-judgment:v3 {"issued": 1} -->"#,
            );
            at
        },
        healed: "",
        lands: true,
    },
    Row {
        mark: "review",
        arm: |c| {
            let at = comments(c);
            c.hub.flow_comment(
                REPO,
                3,
                crate::sim::CONNECTOR,
                "<!-- codex-review-state:v1 {not json} -->",
            );
            at
        },
        healed: "",
        lands: true,
    },
    Row {
        mark: "release_age",
        arm: |c| {
            let at = comments(c);
            renovate(
                c,
                &format!("{RELEASE}\n```\n```\n{}", RELEASE.replace("01-02", "02-30")),
            );
            at
        },
        healed: "Artifact update problem\n```\nlock-age: nothing in the refresh is old enough until 2099-01-02T00:00:00Z\n```\n",
        lands: false,
    },
];

/// The row's whole story, panicking at the first thing that doesn't hold.
fn run(row: &Row) {
    let mut c = Case::new();
    let now = jobs::now() + 10_000;
    // In the ledger, which a degradation is recorded against, and then
    // reviewed clean.
    c.pass(now - 1, false);
    c.clean_review();
    let at = (row.arm)(&mut c);
    let what = mark(row.mark);
    for pass in 0..2 {
        let reports = c.pass(now + pass, false);
        let three = decided(&reports, 3);
        assert!(names_only(&three, &what), "{three}");
        // Counted failed: it waits on it at no stage, and nothing lands.
        assert_eq!(three["step"], json!({"Wait": {"until": null}}), "{three}");
        assert!(three["stage"].is_null(), "{three}");
        assert_eq!(c.hub.count(crate::sim::Call::Merge), 0, "{three}");
        assert!(c.record().live.is_empty(), "{three}");
        // Recorded once while it lasts.
        assert_eq!(naming(&c, "degraded", &what), 1);
    }
    // `cn flow status` names it from the last decision.
    let shown = serde_json::to_value(
        status(c.dir.path(), &REPO.parse().unwrap(), &c.config, now + 2).unwrap(),
    )
    .unwrap();
    assert!(
        shown["degraded"]
            .as_array()
            .is_some_and(|d| d.iter().any(|d| d["mark"] == row.mark)),
        "{shown}"
    );
    // Put right: the next read of it clears it, and the PR lands, or for
    // Renovate's wait, waits on it.
    c.hub.edit_flow_comment(REPO, 3, at, row.healed);
    let reports = c.pass(now + 2, false);
    let three = decided(&reports, 3);
    assert_eq!(three.get("degraded"), None, "{three}");
    assert_eq!(naming(&c, "restored", &what), 1);
    assert_eq!(naming(&c, "degraded", &what), 1);
    assert_eq!(
        c.hub.pull(REPO, 3).unwrap().is_merged(),
        row.lands,
        "{three}"
    );
    if !row.lands {
        assert_eq!(three["stage"], "releaseAge", "{three}");
    }
}

#[test]
fn a_mark_that_doesnt_parse_is_recorded_once_counts_failed_and_is_named() {
    let failed: Vec<String> = ROWS
        .iter()
        .enumerate()
        .filter_map(|(i, row)| {
            std::panic::catch_unwind(|| run(row))
                .err()
                .map(|_| format!("row {i} ({})", row.mark))
        })
        .collect();
    assert!(failed.is_empty(), "failed: {}", failed.join(", "));
}

/// A release-age wait that parses, beside a block that's another failure,
/// is that failure, not a mark that didn't parse: what main did.
#[test]
fn a_release_age_comment_with_another_failure_is_no_degradation() {
    let mut c = Case::new();
    let now = jobs::now() + 10_000;
    c.pass(now - 1, false);
    c.clean_review();
    renovate(&mut c, &format!("{RELEASE}\n```\n```\nnetwork failed"));
    let three = decided(&c.pass(now, false), 3);
    assert_eq!(three.get("degraded"), None, "{three}");
    assert_ne!(three["stage"], "releaseAge", "{three}");
}

/// PR 3's work of `purpose`, as its record has it.
fn work(c: &Case, purpose: fn(&jobs::Purpose) -> bool) -> jobs::Work {
    c.record()
        .jobs
        .into_iter()
        .find(|w| purpose(&w.purpose))
        .unwrap()
}

/// The answer it names, degraded: the worker's `job`, doing `kind`.
fn answer(job: &str, kind: &str) -> Value {
    json!({"path": "answer", "job": job, "kind": kind})
}

/// A stop marker that says neither a stop nor the deadline: main took the
/// worker as never stopped and went on, recording nothing.
#[test]
fn a_stop_marker_that_doesnt_parse_is_recorded_once_counts_failed_and_is_named() {
    let c = two(2);
    let now = jobs::now();
    prepared(&c, now, true);
    c.pass(now + 1, false);
    let work = work(&c, |p| matches!(p, jobs::Purpose::Proof));
    let marker = work.path.with_extension("cancel");
    std::fs::write(&marker, b"obsol").unwrap();
    let what = answer(&work.job.id, "proof");
    for pass in 2..4 {
        let three = decided(&c.pass(now + pass, false), 3);
        assert!(names_only(&three, &what), "{three}");
        assert_eq!(three["step"], json!({"Wait": {"until": null}}), "{three}");
        assert_eq!(
            three["degraded"][0]["why"], "its stop marker says neither a stop nor its deadline",
            "{three}"
        );
        assert_eq!(naming(&c, "degraded", &what), 1);
    }
    std::fs::remove_file(&marker).unwrap();
    let three = decided(&c.pass(now + 4, false), 3);
    assert_eq!(three.get("degraded"), None, "{three}");
    assert_eq!(naming(&c, "restored", &what), 1);
}

/// A preparation's answer that answers another kind of work: main took it
/// as a preparation that failed, and prepared again later, recording
/// nothing.
#[test]
fn an_answer_to_another_kind_of_work_is_recorded_once_counts_failed_and_is_named() {
    let c = two(2);
    let now = jobs::now();
    prepared(&c, now, true);
    let work = work(&c, |p| matches!(p, jobs::Purpose::Prepare));
    let path = work.path.with_extension("answer");
    let kept = std::fs::read(&path).unwrap();
    let mut other: Value = serde_json::from_slice(&kept).unwrap();
    let response = other["result"]["Ok"].as_object_mut().unwrap();
    for field in ["ready", "slot", "why"] {
        response.remove(field).unwrap();
    }
    response.insert("kind".into(), json!("proof"));
    response.insert("results".into(), json!([]));
    std::fs::write(&path, serde_json::to_vec(&other).unwrap()).unwrap();
    let what = answer(&work.job.id, "prepare");
    for pass in 1..3 {
        let three = decided(&c.pass(now + pass, false), 3);
        assert!(names_only(&three, &what), "{three}");
        assert_eq!(three["step"], json!({"Wait": {"until": null}}), "{three}");
        assert_eq!(
            three["degraded"][0]["why"], "it answers another kind of work",
            "{three}"
        );
        assert_eq!(naming(&c, "degraded", &what), 1);
    }
    std::fs::write(&path, &kept).unwrap();
    let three = decided(&c.pass(now + 3, false), 3);
    assert_eq!(three.get("degraded"), None, "{three}");
    assert_eq!(naming(&c, "restored", &what), 1);
}
