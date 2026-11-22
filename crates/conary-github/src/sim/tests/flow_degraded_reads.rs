//! A read of GitHub that fails or stops short is a degraded path too
//! (`docs/flow.md`, "Degraded paths"): recorded once while it lasts,
//! counted as its check's failure, named in the PR's decision, and cleared
//! once the same read answers. Read as JSON, as `cn flow` and a ledger
//! reader see them, so the table runs on a tree that has no such path.
use super::flow_degraded::{decided, ops};
use super::flow_pass_case::{Case, REPO};
use super::head;
use crate::api::Source;
use crate::flow::*;
use crate::sim::{Call, Hub};
use serde_json::{Value, json};

/// The ops of kind `op` that name the read `read`.
fn naming(c: &Case, op: &str, read: &str) -> usize {
    ops(c, op)
        .iter()
        .filter(|v| {
            let body = v.pointer("/op/body").or(v.pointer("/body")).unwrap();
            body["what"] == json!({"path": "read", "read": read})
        })
        .count()
}
fn names(next: &Value, read: &str) -> bool {
    next["degraded"]
        .as_array()
        .is_some_and(|d| d.iter().any(|d| d["path"] == "read" && d["read"] == read))
}

/// The next call of `call` fails, and only it.
fn once(h: &Hub, call: Call) {
    h.fail(call);
    h.race(call, move |h| h.race(call, move |h| h.heal(call)));
}

/// A page more of green check suites on the PR's head, so its snapshot
/// reads them past the first.
fn page_of_suites(h: &Hub) {
    for i in 0..crate::api::PER_PAGE {
        h.check_run(&head(3), &format!("shard {i}"), "success", None);
    }
}

/// The PR's stored snapshot, edited.
fn snap(h: &Hub, edit: fn(&mut Snapshot)) {
    let mut snapshot = h.snapshot(REPO, 3).unwrap();
    edit(&mut snapshot);
    h.flow_snapshot(REPO, 3, snapshot);
}

/// One read of the family: how a test makes it fail or stop short this
/// pass, the read the decision names, and how GitHub answers again, after
/// which the PR lands.
struct Row {
    read: &'static str,
    arm: fn(&Hub),
    heal: fn(&Hub),
}

/// Each read a reviewed, green PR's decision takes, failing or stopping
/// short while every other answers. Main went on past the swallowed ones as
/// if they'd answered, or failed the PR with GitHub's error, and recorded
/// none.
const ROWS: &[Row] = &[
    // Reads the decision can't go on without: it waits, naming the read.
    Row {
        read: "pull",
        arm: |h| h.fail(Call::FlowSnapshot),
        heal: |h| h.heal(Call::FlowSnapshot),
    },
    Row {
        read: "base",
        arm: |h| h.fail(Call::Branch),
        heal: |h| h.heal(Call::Branch),
    },
    Row {
        read: "files",
        arm: |h| h.fail(Call::Files),
        heal: |h| h.heal(Call::Files),
    },
    Row {
        read: "checks",
        arm: |h| h.fail(Call::Statuses),
        heal: |h| h.heal(Call::Statuses),
    },
    Row {
        read: "reviews",
        arm: |h| h.fail(Call::Comments),
        heal: |h| h.heal(Call::Comments),
    },
    // Reads it goes on past, holding what they were for failed.
    // A page of the head's check suites past the first doesn't read: the
    // PR's snapshot hasn't, which main took as all its checks and a cut.
    Row {
        read: "pull",
        arm: |h| {
            page_of_suites(h);
            h.fail(Call::CheckSuites);
        },
        heal: |h| h.heal(Call::CheckSuites),
    },
    Row {
        read: "commits",
        arm: |h| h.fail(Call::CompareCommits),
        heal: |h| h.heal(Call::CompareCommits),
    },
    Row {
        read: "merge_tree",
        arm: |h| h.fail(Call::Commit),
        heal: |h| h.heal(Call::Commit),
    },
    // Who put the owner label on, the steward's listing with no mark of the
    // flow's: while it can't be told the label is neither listed nor gone
    // past, and the PR lands once it reads as an App's.
    Row {
        read: "owner_label",
        arm: |h| {
            h.label_as(REPO, 3, &["needs-peter"], (crate::sim::STEWARD, "Bot"));
            // The decision's read fails, the class's read of the timeline
            // after it answers, and every scope check's read fails again.
            h.fail(Call::Timeline);
            h.race(Call::Timeline, |h| {
                h.race(Call::Timeline, |h| {
                    h.heal(Call::Timeline);
                    h.race(Call::Timeline, |h| h.fail(Call::Timeline));
                })
            });
        },
        heal: |h| h.heal(Call::Timeline),
    },
    // The flow's own wait, named, where main handed it to the owner: it
    // lands once GitHub gives the snapshot in full.
    Row {
        read: "snapshot",
        arm: |h| snap(h, |s| s.complete = false),
        heal: |h| snap(h, |s| s.complete = true),
    },
    // A run whose suite a snapshot behind GitHub doesn't hold: whether its
    // workflow is still running can't be told, where main counted it green.
    Row {
        read: "checks",
        arm: |h| {
            h.stale(Call::FlowSnapshot);
            h.check_run(&head(3), "late", "success", None);
        },
        heal: |h| h.fresh(Call::FlowSnapshot),
    },
    // Evidence's shared reads, failing once the decision's own answered:
    // the reviews after a check's attestation is read, and the head's
    // checks after the decision's read of its comments.
    Row {
        read: "reviews",
        arm: |h| h.race(Call::Artifacts, |h| once(h, Call::Reviews)),
        heal: |_| {},
    },
    Row {
        read: "observations",
        arm: |h| {
            h.race(Call::Comments, |h| {
                h.race(Call::Comments, |h| once(h, Call::FlowSnapshot))
            })
        },
        heal: |_| {},
    },
    // Evidence's re-read of the head's checks fails past its first page of
    // suites, and only it.
    Row {
        read: "observations",
        arm: |h| {
            page_of_suites(h);
            h.race(Call::Comments, |h| {
                h.race(Call::Comments, |h| once(h, Call::CheckSuites))
            })
        },
        heal: |_| {},
    },
];

/// The row's whole story, panicking at the first thing that doesn't hold.
fn run(row: &Row) {
    let c = Case::new();
    let now = jobs::now() + 10_000;
    // In the ledger, which a degradation is recorded against, and then
    // reviewed clean.
    c.pass(now - 1, false);
    c.clean_review();
    for pass in 0..2 {
        (row.arm)(&c.hub);
        let reports = c.pass(now + pass, false);
        let three = decided(&reports, 3);
        assert!(names(&three, row.read), "{three}");
        // Counted failed: nothing is granted, and nothing lands.
        assert!(
            !matches!(three["step"].as_str(), Some("Merge" | "JoinTrain")),
            "{three}"
        );
        assert_eq!(c.hub.count(Call::Merge), 0, "{three}");
        assert!(c.record().live.is_empty(), "{three}");
        // Recorded once while it lasts.
        assert_eq!(naming(&c, "degraded", row.read), 1);
        (row.heal)(&c.hub);
    }
    // `cn flow status` names it from the last decision.
    let shown = serde_json::to_value(
        status(c.dir.path(), &REPO.parse().unwrap(), &c.config, now + 2).unwrap(),
    )
    .unwrap();
    assert!(
        shown["degraded"]
            .as_array()
            .is_some_and(|d| d.iter().any(|d| d["read"] == row.read)),
        "{shown}"
    );
    // GitHub answers again: the same read clears it, and the PR lands.
    let reports = c.pass(now + 2, false);
    let three = decided(&reports, 3);
    assert!(!names(&three, row.read), "{three}");
    assert_eq!(naming(&c, "restored", row.read), 1);
    assert_eq!(naming(&c, "degraded", row.read), 1);
    assert!(c.hub.pull(REPO, 3).unwrap().is_merged(), "{three}");
}

#[test]
fn a_read_that_fails_is_recorded_once_counts_failed_and_is_named() {
    let failed: Vec<String> = ROWS
        .iter()
        .enumerate()
        .filter_map(|(i, row)| {
            std::panic::catch_unwind(|| run(row))
                .err()
                .map(|_| format!("row {i} ({})", row.read))
        })
        .collect();
    assert!(failed.is_empty(), "failed: {}", failed.join(", "));
}

/// A pass kicked for PR 4 alone, so it doesn't visit PR 3.
fn kick(c: &Case, now: u64) {
    let mut store = c.store.borrow_mut();
    let pass = Pass {
        config: &c.config,
        policy: &c.policy,
        root: c.dir.path(),
        launcher: &c.deferred,
        now,
        dry_run: false,
        ledger: None,
    };
    pass.run(&c.hub, &REPO.parse().unwrap(), Some(4), Some(&mut store))
        .unwrap();
}

/// What occupancy's reads of a PR the pass doesn't visit meet: GitHub's,
/// and the pause's. Main reserved every slot for it and recorded nothing.
#[test]
fn an_unvisited_pr_s_reads_that_fail_are_recorded() {
    let c = Case::new();
    let now = jobs::now();
    c.pass(now, false);
    c.hub.fail(Call::Files);
    kick(&c, now + 1);
    assert_eq!(naming(&c, "degraded", "files"), 1);
    c.hub.heal(Call::Files);
    kick(&c, now + 2);
    assert_eq!(naming(&c, "restored", "files"), 1);
    let pause = crate::flow::persist::dir(c.dir.path(), &REPO.parse().unwrap()).join("pause");
    std::fs::write(&pause, b"{\"scope\":\"everything\"}").unwrap();
    kick(&c, now + 3);
    assert_eq!(naming(&c, "degraded", "pause"), 1);
    std::fs::remove_file(&pause).unwrap();
    kick(&c, now + 4);
    assert_eq!(naming(&c, "restored", "pause"), 1);
}

/// A PR the pass doesn't visit, holding a live grant (a train seat, the
/// permission that outlasts its pass), whose read fails: main kept the
/// grant, since only a PR read closed withdrew it.
#[test]
fn an_unvisited_pr_whose_read_fails_loses_its_grant() {
    let mut c = Case::new();
    c.config.settings.train = true;
    c.clean_review();
    let now = jobs::now();
    c.pass(now, false);
    assert!(c.record().live.contains(&head(3)));
    c.hub.fail(Call::Pull.pr(3));
    kick(&c, now + 1);
    assert!(c.record().live.is_empty());
    assert_eq!(naming(&c, "degraded", "pull"), 1);
    // The PR reads again: its decision's read clears it.
    c.hub.heal(Call::Pull);
    c.pass(now + 2, false);
    assert_eq!(naming(&c, "restored", "pull"), 1);
}

/// A read that answered the decision and fails when its permission is
/// about to be used: the merge tree, which nothing else the scope compares
/// shows. Main merged.
#[test]
fn a_read_that_fails_before_permission_is_used_withdraws_it() {
    let c = Case::new();
    let now = jobs::now() + 10_000;
    c.pass(now - 1, false);
    c.clean_review();
    // Occupancy's read, the decision's, then the scope's.
    c.hub.race(Call::Commit, |h| {
        h.race(Call::Commit, |h| {
            h.race(Call::Commit, |h| once(h, Call::Commit))
        })
    });
    let reports = c.pass(now, false);
    let three = reports.iter().find(|r| r.pr == 3).unwrap();
    assert!(!c.hub.pull(REPO, 3).unwrap().is_merged(), "{three:?}");
    assert!(
        three
            .error
            .as_deref()
            .is_some_and(|e| e.contains("GitHub's test merge")),
        "{three:?}"
    );
    // Recorded once, and the decision saved waits naming it, as `cn flow
    // status` shows. At 6c2c006 neither: the saved decision said Merge.
    assert_eq!(naming(&c, "degraded", "merge_tree"), 1);
    let saved = serde_json::to_value(c.record().decision.unwrap()).unwrap();
    assert!(names(&saved, "merge_tree"), "{saved}");
    assert!(saved["stage"].is_null(), "{saved}");
    let shown = serde_json::to_value(
        status(c.dir.path(), &REPO.parse().unwrap(), &c.config, now + 1).unwrap(),
    )
    .unwrap();
    assert!(
        shown["degraded"]
            .as_array()
            .is_some_and(|d| d.iter().any(|d| d["read"] == "merge_tree")),
        "{shown}"
    );
    // It reads again: cleared, and once the failed step's retry is due,
    // the PR lands.
    c.pass(now + 1, false);
    assert_eq!(naming(&c, "restored", "merge_tree"), 1);
    c.pass(now + 86_400, false);
    assert!(c.hub.pull(REPO, 3).unwrap().is_merged());
}

/// A read the decision can't go on without that fails after others met or
/// answered: what they met is recorded and named with it, and what they
/// answered is cleared. At 762e522 only the failing read was kept.
#[test]
fn the_reads_before_one_that_fails_are_kept() {
    let c = Case::new();
    let now = jobs::now() + 10_000;
    c.pass(now - 1, false);
    c.clean_review();
    // A run's suite isn't in the snapshot, then the comments don't read.
    c.hub.stale(Call::FlowSnapshot);
    c.hub.check_run(&head(3), "late", "success", None);
    c.hub.fail(Call::Comments);
    let three = decided(&c.pass(now, false), 3);
    assert!(
        names(&three, "reviews") && names(&three, "checks"),
        "{three}"
    );
    assert_eq!(naming(&c, "degraded", "reviews"), 1);
    assert_eq!(naming(&c, "degraded", "checks"), 1);
    c.hub.fresh(Call::FlowSnapshot);
    c.hub.heal(Call::Comments);
    // The PR doesn't read; then it does, and its files don't.
    c.hub.fail(Call::FlowSnapshot);
    c.pass(now + 1, false);
    assert_eq!(naming(&c, "degraded", "pull"), 1);
    c.hub.heal(Call::FlowSnapshot);
    c.hub.fail(Call::Files);
    let three = decided(&c.pass(now + 2, false), 3);
    assert_eq!(naming(&c, "restored", "pull"), 1, "{three}");
    assert!(!names(&three, "pull"), "{three}");
    assert!(names(&three, "files"), "{three}");
}

/// The `n`th call of `call` fails, and only it.
fn at(h: &Hub, call: Call, n: usize) {
    if n <= 1 {
        once(h, call)
    } else {
        h.race(call, move |h| at(h, call, n - 1))
    }
}

/// A read the decision can't go on without that answered it and fails at
/// the scope's re-read: the permission is withdrawn, recorded and named, as
/// a read the decision goes on past is. At b18e84e the error reached the
/// pass as a plain read error: refused, but nothing recorded and the saved
/// decision said Merge.
#[test]
fn a_required_read_that_fails_before_permission_is_used_is_recorded() {
    // Which call of it is the scope's re-read.
    let mut failed = Vec::new();
    for (call, read, n) in [(Call::Pulls, "pulls", 5), (Call::Branch, "base", 4)] {
        let c = Case::new();
        let now = jobs::now() + 10_000;
        c.pass(now - 1, false);
        c.clean_review();
        at(&c.hub, call, n);
        let reports = c.pass(now, false);
        let three = reports.iter().find(|r| r.pr == 3).unwrap();
        let saved = serde_json::to_value(c.record().decision.unwrap()).unwrap();
        let withdrawn = three
            .error
            .as_deref()
            .is_some_and(|e| e.contains("before permission was used"));
        if c.hub.pull(REPO, 3).unwrap().is_merged()
            || !withdrawn
            || naming(&c, "degraded", read) != 1
            || !names(&saved, read)
            || !saved["stage"].is_null()
        {
            failed.push(format!("{read}: {three:?} {saved}"));
            continue;
        }
        // It reads again: cleared, and the PR lands.
        c.hub.heal(call);
        c.pass(now + 1, false);
        c.pass(now + 86_400, false);
        if naming(&c, "restored", read) != 1 || !c.hub.pull(REPO, 3).unwrap().is_merged() {
            failed.push(format!("{read}: not restored and landed"));
        }
    }
    assert!(failed.is_empty(), "failed: {failed:#?}");
}
