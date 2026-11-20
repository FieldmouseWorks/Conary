//! A degraded outcome is recorded in the ledger, counts as its check's
//! failure and is named in the PR's decision, until the path works again
//! (`docs/flow.md`, "Degraded paths"). These read the decision and the op
//! log as JSON, as `cn flow` and a ledger reader see them.
use super::flow_pass_case::{Case, REPO};
use super::flow_pass_slots::{prepared, step, two};
use crate::api::Source;
use crate::flow::*;
use crate::sim::Call;
use serde_json::{Value, json};

/// The op log's ops of kind `op`, as written.
pub(super) fn ops(c: &Case, op: &str) -> Vec<Value> {
    let path = conary_core::log::log_path(c.store.borrow().lock().dir());
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|v| v.pointer("/op/body/op").or(v.pointer("/body/op")) == Some(&json!(op)))
        .collect()
}
/// The PR's decision, as JSON. A degraded path it waits on at no stage (a
/// record or answer that didn't read, or a read it couldn't go on without)
/// is the PR's error too, so the pass is booked again to read it again: the
/// description, and for a read, GitHub's error after it. Anything else is
/// none.
pub(super) fn decided(reports: &[Report], pr: u64) -> Value {
    let report = reports.iter().find(|r| r.pr == pr).unwrap();
    let next = serde_json::to_value(report.next.as_ref().unwrap()).unwrap();
    let description = next["description"].as_str().unwrap();
    let waits = next["stage"].is_null() && description.starts_with("Degraded: ");
    let read = next["degraded"][0]["path"] == "read";
    match &report.error {
        Some(error) => assert!(
            waits
                && if read {
                    error.starts_with(&format!("{description}; "))
                } else {
                    error == description
                },
            "#{pr}: {error}"
        ),
        None => assert!(!waits, "#{pr}: {next}"),
    }
    next
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
            five["description"]
                .as_str()
                .unwrap()
                .contains("#3's record didn't read, so every slot is held"),
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
    // Read again: a dry run names nothing and writes nothing, and a real
    // pass's later op clears it, and the first stays.
    std::fs::write(&path, &kept).unwrap();
    let three = decided(&c.pass(now + 4, true), 3);
    assert_eq!(three.get("degraded"), None, "{three}");
    assert!(ops(&c, "restored").is_empty());
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
    let three = decided(&c.pass(now + 4, true), 3);
    assert_eq!(three.get("degraded"), None, "{three}");
    assert!(ops(&c, "restored").is_empty());
    let three = decided(&c.pass(now + 4, false), 3);
    assert_eq!(three.get("degraded"), None, "{three}");
    assert_eq!(ops(&c, "restored").len(), 1);
}

/// The review-settled successes on PR 3's head.
fn granted(c: &Case) -> usize {
    c.hub
        .statuses(REPO, &super::head(3))
        .unwrap()
        .iter()
        .filter(|s| s.context == SETTLED && s.state == "success")
        .count()
}
/// A reviewed PR, a fix worker's answer posted and on disk, in the train
/// (`seated`) or landing directly. Returns the answer's path.
fn posted_answer(c: &mut Case, seated: bool, now: u64) -> std::path::PathBuf {
    c.clean_review();
    c.config.settings.train = seated;
    if seated {
        let reports = c.pass(now, false);
        assert_eq!(step(&reports, 0).1, Step::JoinTrain, "{reports:?}");
    }
    let mut record = c.record();
    let path = c.dir.path().join("3-90.job");
    record.jobs.push(jobs::Work {
        purpose: jobs::Purpose::Fix(Kind::Findings),
        job: Job {
            id: "3-90".into(),
            revision: Revision::from(&c.hub.pull(REPO, 3).unwrap()),
            state: JobState::Done,
            started: now,
            repeated: false,
        },
        path: path.clone(),
        deadline: now + 10_000,
        posted: true,
        recipes: Default::default(),
        slotless: false,
        handed: vec![],
        sent: Default::default(),
    });
    let file = record_path(c, 3);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, serde_json::to_vec(&record).unwrap()).unwrap();
    let answer = path.with_extension("answer");
    let completion = jobs::Completion {
        at: now,
        result: Some(Ok(conary_core::plugin::Response {
            contract: conary_core::plugin::CONTRACT.into(),
            by: conary_core::plugin::Identity {
                engine: "fixture".into(),
                session: None,
            },
            execution: None,
            answer: conary_core::plugin::Answer::Fix {
                result: conary_core::plugin::FixResult::Unchanged,
                answers: Some(vec![]),
            },
        })),
    };
    std::fs::write(&answer, serde_json::to_vec(&completion).unwrap()).unwrap();
    answer
}

/// A seated PR whose posted worker's answer doesn't read: it
/// waits, named degraded, and its seat is granted nothing, where the train
/// stage's wait granted it before.
#[test]
fn a_seated_pr_with_an_unreadable_answer_is_granted_nothing() {
    let mut c = Case::new();
    let now = jobs::now();
    let answer = posted_answer(&mut c, true, now);
    std::fs::write(&answer, b"corrupt completion").unwrap();
    let before = granted(&c);
    let reports = c.pass(now + 1, false);
    assert_eq!(granted(&c), before, "permission published: {reports:?}");
    assert!(c.record().live.is_empty(), "{reports:?}");
    assert_eq!(c.hub.count(Call::Land), 0);
    let three = decided(&reports, 3);
    assert_eq!(three["step"], json!({"Wait": {"until": null}}), "{three}");
    assert_ne!(three["stage"], "train", "{three}");
    assert_eq!(three["degraded"][0]["path"], "answer", "{three}");
    assert_eq!(ops(&c, "degraded").len(), 1);
}

/// An answer that stops reading during the pass, landing directly and in
/// the train: after occupancy read it and before the PR's decision did
/// (on the PR's snapshot read), or after the decision and before its
/// permission is used (on the stage write between the two scope checks).
/// Either way nothing is granted or landed, and the report names it.
#[test]
fn an_answer_that_stops_reading_during_the_pass_is_granted_nothing() {
    for on in [Call::FlowSnapshot, Call::FlowStage] {
        for seated in [false, true] {
            let mut c = Case::new();
            let now = jobs::now();
            let answer = posted_answer(&mut c, seated, now);
            let before = granted(&c);
            c.hub.race(crate::sim::On::from(on), move |_| {
                std::fs::write(&answer, b"corrupt completion").unwrap();
            });
            let reports = c.pass(now + 1, false);
            let case = format!("{on:?} seated {seated}: {reports:?}");
            assert_eq!(granted(&c), before, "permission published: {case}");
            assert!(c.record().live.is_empty(), "{case}");
            assert_eq!(c.hub.count(Call::Land), 0, "{case}");
            let error = reports[0].error.as_deref().unwrap_or_default();
            assert!(error.contains("answer didn't read"), "{case}");
            if on == Call::FlowSnapshot {
                // The decision met it: it waits on it, and it's recorded.
                let three = decided(&reports, 3);
                assert_eq!(three["degraded"][0]["path"], "answer", "{case}");
                assert_eq!(ops(&c, "degraded").len(), 1, "{case}");
            }
        }
    }
}
