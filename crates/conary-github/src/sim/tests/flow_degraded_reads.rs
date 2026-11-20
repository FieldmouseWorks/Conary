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

/// One read of the family: how a test makes it fail this pass, what the
/// decision names, and the calls that heal it, or catch up.
struct Row {
    read: &'static str,
    arm: fn(&Hub),
    heal: &'static [Call],
}

/// Each read a reviewed, green PR's decision takes, failing while every
/// other answers. Main went on past the swallowed ones as if they'd
/// answered, or failed the PR with GitHub's error, and recorded none.
const ROWS: &[Row] = &[
    // Reads the decision can't go on without: it waits, naming the read.
    Row {
        read: "pull",
        arm: |h| h.fail(Call::FlowSnapshot),
        heal: &[Call::FlowSnapshot],
    },
    Row {
        read: "base",
        arm: |h| h.fail(Call::Branch),
        heal: &[Call::Branch],
    },
    Row {
        read: "files",
        arm: |h| h.fail(Call::Files),
        heal: &[Call::Files],
    },
    Row {
        read: "checks",
        arm: |h| h.fail(Call::Statuses),
        heal: &[Call::Statuses],
    },
    Row {
        read: "reviews",
        arm: |h| h.fail(Call::Comments),
        heal: &[Call::Comments],
    },
    // Reads it goes on past, holding what they were for failed.
    Row {
        read: "commits",
        arm: |h| h.fail(Call::CompareCommits),
        heal: &[Call::CompareCommits],
    },
    // A run whose suite a snapshot behind GitHub doesn't hold: whether its
    // workflow is still running can't be told, where main counted it green.
    Row {
        read: "checks",
        arm: |h| {
            h.stale(Call::FlowSnapshot);
            h.check_run(&head(3), "late", "success", None);
        },
        heal: &[Call::FlowSnapshot],
    },
    // Evidence's shared reads, failing once the decision's own answered:
    // the reviews after a check's attestation is read, and the head's
    // checks after the decision's read of its comments.
    Row {
        read: "reviews",
        arm: |h| h.race(Call::Artifacts, |h| once(h, Call::Reviews)),
        heal: &[],
    },
    Row {
        read: "observations",
        arm: |h| {
            h.race(Call::Comments, |h| {
                h.race(Call::Comments, |h| once(h, Call::FlowSnapshot))
            })
        },
        heal: &[],
    },
];

#[test]
fn a_read_that_fails_is_recorded_once_counts_failed_and_is_named() {
    for row in ROWS {
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
            assert!(names(&three, row.read), "{}: {three}", row.read);
            // Counted failed: nothing is granted, and nothing lands.
            assert!(
                !matches!(
                    three["step"].as_str(),
                    Some("Merge" | "Settle" | "JoinTrain")
                ),
                "{}: {three}",
                row.read
            );
            assert_eq!(c.hub.count(Call::Merge), 0, "{}: {three}", row.read);
            assert!(c.record().live.is_empty(), "{}: {three}", row.read);
            // Recorded once while it lasts.
            assert_eq!(naming(&c, "degraded", row.read), 1, "{}", row.read);
            for call in row.heal {
                c.hub.heal(*call);
                c.hub.fresh(*call);
            }
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
            "{}: {shown}",
            row.read
        );
        // GitHub answers again: the same read clears it, and the PR lands.
        let reports = c.pass(now + 2, false);
        let three = decided(&reports, 3);
        assert!(!names(&three, row.read), "{}: {three}", row.read);
        assert_eq!(naming(&c, "restored", row.read), 1, "{}", row.read);
        assert_eq!(naming(&c, "degraded", row.read), 1, "{}", row.read);
        assert!(
            c.hub.pull(REPO, 3).unwrap().is_merged(),
            "{}: {three}",
            row.read
        );
    }
}

/// A PR the pass doesn't visit, holding a live grant, whose read fails:
/// main kept the grant, since only a PR read closed withdrew it.
#[test]
fn an_unvisited_pr_whose_read_fails_loses_its_grant() {
    let mut c = Case::new();
    c.as_bot();
    c.clean_review();
    let now = jobs::now();
    c.pass(now, false);
    assert!(c.record().live.contains(&head(3)));
    c.hub.fail(Call::Pull.pr(3));
    {
        let mut store = c.store.borrow_mut();
        let pass = Pass {
            config: &c.config,
            policy: &c.policy,
            root: c.dir.path(),
            launcher: &c.deferred,
            now: now + 1,
            dry_run: false,
            ledger: None,
        };
        pass.run(&c.hub, &REPO.parse().unwrap(), Some(4), Some(&mut store))
            .unwrap();
    }
    assert!(c.record().live.is_empty());
    assert_eq!(naming(&c, "degraded", "pull"), 1);
    // The PR reads again: its decision's read clears it.
    c.hub.heal(Call::Pull);
    c.pass(now + 2, false);
    assert_eq!(naming(&c, "restored", "pull"), 1);
}
