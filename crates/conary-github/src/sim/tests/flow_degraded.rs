//! A degraded outcome is recorded in the ledger, counts as its check's
//! failure and is named in the PR's decision, until the path works again
//! (`docs/flow.md`, "Degraded paths"). These read the decision and the op
//! log as JSON, as `cn flow` and a ledger reader see them.
use super::flow_pass_case::{Case, REPO};
use super::flow_pass_slots::{prepared, two};
use crate::flow::*;
use crate::sim::Call;
use serde_json::{Value, json};

/// The op log's ops of kind `op`, as written.
fn ops(c: &Case, op: &str) -> Vec<Value> {
    let path = conary_core::log::log_path(c.store.borrow().lock().dir());
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|v| v.pointer("/op/body/op").or(v.pointer("/body/op")) == Some(&json!(op)))
        .collect()
}
fn decided(reports: &[Report], pr: u64) -> Value {
    let report = reports.iter().find(|r| r.pr == pr).unwrap();
    assert_eq!(report.error, None, "#{pr}");
    serde_json::to_value(report.next.as_ref().unwrap()).unwrap()
}
fn record_path(c: &Case, pr: u64) -> std::path::PathBuf {
    c.dir
        .path()
        .join("flow")
        .join(REPO.to_lowercase())
        .join(format!("{pr}.json"))
}

/// An unreadable proof ownership record: main reserved every slot for it
/// and failed its own PR with an error, with nothing in the ledger, and
/// once the record read again nothing showed it had happened.
#[test]
fn an_unreadable_record_is_recorded_once_counts_failed_and_is_named() {
    let c = two(2);
    let now = jobs::now();
    // Both PRs are in the ledger, and each is ready to prove, needing a slot.
    prepared(&c, now, true);
    let path = record_path(&c, 3);
    let kept = std::fs::read(&path).unwrap();
    std::fs::write(&path, b"{\"memory\": not json").unwrap();
    for pass in 0..2 {
        let reports = c.pass(now + 1 + pass, false);
        let three = decided(&reports, 3);
        assert_eq!(three["step"], json!({"Wait": {"until": null}}), "{three}");
        assert_eq!(three["degraded"][0]["path"], "record", "{three}");
        assert!(
            three["description"]
                .as_str()
                .unwrap()
                .starts_with("Degraded: its flow record didn't read"),
            "{three}"
        );
        // Its work holds every slot, and the PR waiting on one says whose
        // record it is.
        let five = decided(&reports, 5);
        assert_eq!(five["stage"], "proofSlot", "{five}");
        assert!(
            five["description"].as_str().unwrap().contains("#3's work"),
            "{five}"
        );
        // Recorded once while it lasts, never written over.
        assert_eq!(ops(&c, "degraded").len(), 1);
        assert_eq!(std::fs::read(&path).unwrap(), b"{\"memory\": not json");
    }
    let op = &ops(&c, "degraded")[0];
    let body = op.pointer("/op/body").or(op.pointer("/body")).unwrap();
    assert_eq!(body["what"], json!({"path": "record"}));
    assert!(
        body["why"]
            .as_str()
            .unwrap()
            .starts_with("it isn't what it should hold"),
        "{body}"
    );
    // `cn flow status` names it, from local state alone, and so goes on
    // past the record it can't read.
    let status = |at| {
        serde_json::to_value(status(c.dir.path(), &REPO.parse().unwrap(), &c.config, at).unwrap())
            .unwrap()
    };
    let shown = status(now + 2);
    assert_eq!(shown["degraded"][0]["pr"], 3, "{shown}");
    assert_eq!(shown["degraded"][0]["path"], "record", "{shown}");
    // A dry run names it too and writes nothing.
    let three = decided(&c.pass(now + 3, true), 3);
    assert_eq!(three["degraded"][0]["path"], "record");
    assert_eq!(ops(&c, "degraded").len(), 1);
    // Read again: a later op clears it, and the first stays.
    std::fs::write(&path, &kept).unwrap();
    let reports = c.pass(now + 4, false);
    assert_eq!(decided(&reports, 3).get("degraded"), None);
    assert_eq!(ops(&c, "restored").len(), 1);
    assert_eq!(ops(&c, "degraded").len(), 1);
    assert_eq!(status(now + 4).get("degraded"), None);
}

/// An evidence lookup that can't tell: main waited with GitHub's error as
/// its description, recorded nothing, and once it read again nothing
/// showed it had happened.
#[test]
fn an_evidence_lookup_that_cant_tell_is_recorded_once_counts_failed_and_is_named() {
    let c = Case::new();
    c.clean_review();
    let now = jobs::now() + 10_000;
    c.hub.fail(Call::Artifacts);
    for pass in 0..2 {
        let reports = c.pass(now + pass, false);
        let three = decided(&reports, 3);
        assert_eq!(three["step"], json!({"Wait": {"until": null}}), "{three}");
        assert_eq!(three["stage"], "proof", "{three}");
        assert_eq!(three["degraded"][0]["path"], "lookup", "{three}");
        assert!(
            three["description"]
                .as_str()
                .unwrap()
                .starts_with("Degraded: "),
            "{three}"
        );
        assert_eq!(ops(&c, "degraded").len(), 1);
        assert_eq!(c.hub.count(Call::Land), 0);
    }
    let op = &ops(&c, "degraded")[0];
    let body = op.pointer("/op/body").or(op.pointer("/body")).unwrap();
    assert_eq!(body["what"]["path"], "lookup");
    // GitHub answers again: the lookup tells, a later op clears it, and the
    // PR goes on to land.
    c.hub.heal(Call::Artifacts);
    let reports = c.pass(now + 2, false);
    assert_eq!(decided(&reports, 3).get("degraded"), None);
    assert_eq!(ops(&c, "restored").len(), 1);
    assert_eq!(ops(&c, "degraded").len(), 1);
}

/// An unreadable worker answer: main failed its PR with an error, and held
/// every slot for it, with nothing in the ledger.
#[test]
fn an_unreadable_answer_is_recorded_once_counts_failed_and_is_named() {
    let c = two(2);
    let now = jobs::now();
    prepared(&c, now, true);
    c.pass(now + 1, false);
    let work = c
        .record()
        .jobs
        .into_iter()
        .find(|w| matches!(w.purpose, jobs::Purpose::Proof))
        .unwrap();
    let answer = work.path.with_extension("answer");
    std::fs::write(&answer, b"corrupt completion").unwrap();
    for pass in 2..4 {
        let three = decided(&c.pass(now + pass, false), 3);
        assert_eq!(three["step"], json!({"Wait": {"until": null}}), "{three}");
        assert_eq!(
            three["degraded"],
            json!([{"path": "answer", "job": work.job.id, "kind": "proof",
                "why": "it isn't what it should hold: Syntax error at line 1, column 1"}]),
            "{three}"
        );
        assert_eq!(ops(&c, "degraded").len(), 1);
    }
    std::fs::remove_file(&answer).unwrap();
    let three = decided(&c.pass(now + 4, false), 3);
    assert_eq!(three.get("degraded"), None, "{three}");
    assert_eq!(ops(&c, "restored").len(), 1);
}
