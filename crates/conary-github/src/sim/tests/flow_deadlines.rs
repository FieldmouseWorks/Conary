//! A check whose proof-plugin invocation reaches its deadline twice on the
//! same tree is a stall the flow reports, never a third start
//! (`docs/flow.md`, "Deadlines").
use super::flow_pass_case::{Case, REPO, example};
use super::flow_pass_slots::work;
use super::head;
use crate::flow::jobs::{Purpose, Work};
use crate::flow::*;
use serde_json::json;

/// The proof timeout these cases run with.
const TIMEOUT: u64 = 600;

/// PR 3, reviewed clean on a green head, its `machine` check pending: a
/// program the proof plugin runs, with one proof slot.
fn case() -> Case {
    let mut c = Case::new();
    c.config.settings.proof_slots = Some(1);
    c.config.settings.proof_timeout = TIMEOUT;
    c.config
        .settings
        .proofs
        .insert("machine".into(), Recipe::Run(vec!["check".into()]));
    c.config.plugins.proof = Some(example());
    c.clean_review();
    pending(&c, &head(3));
    c
}
fn pending(c: &Case, sha: &str) {
    c.hub
        .post_status(sha, json!({"context":"machine","state":"pending"}));
}
/// PR 3's proof-plugin workers of `purpose`, in the order they started.
fn launched(c: &Case, purpose: fn(&Purpose) -> bool) -> Vec<Work> {
    work(c, 3, purpose)
}
fn prepares(p: &Purpose) -> bool {
    matches!(p, Purpose::Prepare)
}
fn proofs(p: &Purpose) -> bool {
    matches!(p, Purpose::Proof)
}
/// Real passes from `t` on, each when the last one said to wake (a minute
/// on when it named no time), answering every preparation ready when
/// `ready`. Every other worker the plugin is asked for never answers.
/// Stops once it's stalled, or `purpose` has `limit` workers, or after
/// forty passes.
fn drive(c: &Case, t: &mut u64, ready: bool, purpose: fn(&Purpose) -> bool, limit: usize) -> Next {
    let mut next = None;
    for _ in 0..40 {
        if ready {
            c.prepared(true, None);
        }
        let decided = c.pass(*t, false)[0].next.clone().unwrap();
        *t = decided.wait_until.filter(|w| *w > *t).unwrap_or(*t + 60);
        let done =
            decided.description.starts_with("Stalled") || launched(c, purpose).len() >= limit;
        next = Some(decided);
        if done {
            break;
        }
    }
    next.unwrap()
}
/// The stall's report: the check, the tree and both deadlines, overdue at
/// once, the same in a dry pass.
fn stalled(c: &Case, t: u64, next: &Next, hit: &[Work]) {
    assert_eq!(hit.len(), 2, "a third worker started: {next:?}");
    assert_eq!(next.stage, Stage::Proof, "{next:?}");
    assert_eq!(next.step, Step::Wait { until: None });
    let tree = c.hub.tree_at(&c.hub.head_of(REPO, 3));
    for said in [
        "machine",
        &tree[..12],
        &hit[0].deadline.to_string(),
        &hit[1].deadline.to_string(),
    ] {
        assert!(next.description.contains(said), "{said}: {next:?}");
    }
    assert!(next.overdue_at.is_some_and(|at| at <= t), "{next:?}");
    let dry = c.pass(t, true)[0].next.clone().unwrap();
    assert_eq!(
        (dry.stage, dry.step, dry.description),
        (next.stage, next.step.clone(), next.description.clone())
    );
}

/// FP6's regression scenario: a proof plugin that never answers. Its
/// first deadline is retried; its second on the same tree stops the flow,
/// which reports the stall and starts no third worker.
#[test]
fn a_preparation_that_never_answers_stalls_at_its_second_deadline() {
    let c = case();
    let mut t = jobs::now();
    let next = drive(&c, &mut t, false, prepares, 3);
    stalled(&c, t, &next, &launched(&c, prepares));
    // It stays stalled, the same stall, however long it waits.
    for _ in 0..3 {
        t += 3600;
        let again = c.pass(t, false)[0].next.clone().unwrap();
        assert_eq!(again.description, next.description);
        assert_eq!(again.overdue_at, next.overdue_at);
    }
    assert_eq!(launched(&c, prepares).len(), 2);
    assert!(launched(&c, proofs).is_empty());
}

#[test]
fn a_proof_that_never_answers_stalls_at_its_second_deadline() {
    let c = case();
    let mut t = jobs::now();
    let next = drive(&c, &mut t, true, proofs, 3);
    stalled(&c, t, &next, &launched(&c, proofs));
}

/// A preparation's deadline and a proof's are the same check's: one of
/// each on the same tree stalls it.
#[test]
fn a_preparations_deadline_and_a_proofs_add_up() {
    let c = case();
    let mut t = jobs::now();
    drive(&c, &mut t, false, prepares, 2);
    // The first preparation's worker is gone: only the second answers.
    let first = launched(&c, prepares).remove(0).path;
    c.deferred.0.borrow_mut().retain(|p| *p != first);
    let next = drive(&c, &mut t, true, proofs, 2);
    let mut hit = launched(&c, prepares);
    hit.truncate(1);
    hit.extend(launched(&c, proofs));
    stalled(&c, t, &next, &hit);
}

/// Stopped work, here as obsolete, reached no deadline of its own: it
/// counts toward no stall.
#[test]
fn stopped_work_counts_toward_no_stall() {
    let c = case();
    let mut t = jobs::now();
    c.pass(t, false);
    let first = launched(&c, prepares).remove(0);
    std::fs::write(first.path.with_extension("cancel"), b"obsolete\n").unwrap();
    let next = drive(&c, &mut t, false, prepares, 4);
    let all = launched(&c, prepares);
    assert_eq!(all.len(), 3, "{next:?}");
    stalled(&c, t, &next, &all[1..]);
}

/// A deadline hit once is retried as it always was.
#[test]
fn one_deadline_is_retried() {
    let c = case();
    let mut t = jobs::now();
    c.pass(t, false);
    c.prepared(true, None);
    t += 1;
    assert_eq!(c.pass(t, false)[0].next.as_ref().unwrap().step, Step::Prove);
    let first = launched(&c, proofs).remove(0);
    t = first.deadline;
    for _ in 0..10 {
        if launched(&c, proofs).len() == 2 {
            break;
        }
        let next = c.pass(t, false)[0].next.clone().unwrap();
        assert!(!next.description.starts_with("Stalled"), "{next:?}");
        t = next.wait_until.filter(|w| *w > t).unwrap_or(t + 60);
    }
    assert_eq!(launched(&c, proofs).len(), 2);
}

/// A new head on the same tree is the same proof: still stalled. A new
/// tree, or a changed recipe, starts the count again.
#[test]
fn only_a_new_tree_or_recipe_starts_the_count_again() {
    for change in ["same tree", "new tree", "recipe"] {
        let mut c = case();
        let mut t = jobs::now();
        drive(&c, &mut t, false, prepares, 3);
        assert_eq!(launched(&c, prepares).len(), 2, "{change}");
        match change {
            "recipe" => {
                c.config
                    .settings
                    .proofs
                    .insert("machine".into(), Recipe::Run(vec!["check again".into()]));
            }
            _ => {
                if change == "same tree" {
                    let fork = c.hub.parent(&head(3));
                    c.hub.commit_as(&head(4), &[fork], c.hub.patches(&head(3)));
                    assert_eq!(c.hub.tree_at(&head(4)), c.hub.tree_at(&head(3)));
                }
                c.hub.push(REPO, 3, &head(4));
                c.hub.refresh_flow(REPO, 3);
                c.good_ci(&head(4));
                c.clean_review();
                pending(&c, &head(4));
            }
        }
        let next = c.pass(t, false)[0].next.clone().unwrap();
        if change == "same tree" {
            stalled(&c, t, &next, &launched(&c, prepares));
        } else {
            assert_eq!(next.step, Step::Prepare, "{change}: {next:?}");
            assert_eq!(launched(&c, prepares).len(), 3, "{change}");
        }
    }
}

/// A head's tree that can't be read can't tell a stalled tree from a new
/// one: nothing starts, and the read is named degraded.
#[test]
fn a_tree_that_cant_be_read_starts_nothing() {
    let c = case();
    let mut t = jobs::now();
    drive(&c, &mut t, false, prepares, 3);
    c.hub.fail(crate::sim::Call::Tree);
    let next = c.pass(t, false)[0].next.clone().unwrap();
    assert!(matches!(next.step, Step::Wait { .. }), "{next:?}");
    assert_eq!(launched(&c, prepares).len(), 2);
    assert!(
        next.degraded.iter().any(|d| d.what
            == Degraded::Read {
                read: Reading::Tree
            }),
        "{next:?}"
    );
}
