//! The simulated GitHub does what GitHub does, as far as the code under
//! test can tell.

use serde_json::json;

use super::*;
use crate::api::{Pulls, Source};
use crate::branches::{Branches, Merged};
use crate::train::{Forge, Message, PrMerge};
use crate::undo::Reverts;

const TRUNK: &str = "1111111111111111111111111111111111111111";

fn head(n: u64) -> String {
    format!("{n:0>40}")
}

fn hub() -> Hub {
    Hub::new(REMI, TRUNK)
}

fn message() -> Message {
    Message::new(
        "remi: a change",
        3,
        ["remi: a change\n\nCo-authored-by: A <a@b>"],
    )
}

#[test]
fn heads_merged_in_any_order_make_one_tree() {
    let hub = hub();
    for n in [3, 4] {
        hub.pr(REMI, n, &head(n));
    }
    for (branch, order) in [("a", [3, 4]), ("b", [4, 3])] {
        hub.create_branch(REMI, branch, TRUNK).unwrap();
        for n in order {
            assert!(matches!(
                hub.merge(REMI, branch, &head(n), "m").unwrap(),
                Merged::Commit { .. }
            ));
        }
    }
    let tree = |b| hub.tree(REMI, &hub.branch_at(REMI, b).unwrap()).unwrap();
    assert_eq!(tree("a"), tree("b"));
    assert_eq!(tree("a"), tree_of(&[head(3), head(4)]));
    assert_eq!(hub.held(REMI, "a"), [head(3), head(4)]);
    // A head a branch holds already is merged already.
    assert_eq!(
        hub.merge(REMI, "a", &head(3), "m").unwrap(),
        Merged::Already
    );
}

#[test]
fn a_patch_that_clashes_conflicts_only_where_the_other_changed() {
    let hub = hub();
    for n in [3, 4, 5] {
        hub.pr(REMI, n, &head(n));
    }
    hub.clash(&head(4), &head(3));
    hub.create_branch(REMI, "b", TRUNK).unwrap();
    hub.merge(REMI, "b", &head(4), "m").unwrap();
    assert_eq!(
        hub.merge(REMI, "b", &head(3), "m").unwrap(),
        Merged::Conflict
    );
    assert!(matches!(
        hub.merge(REMI, "b", &head(5), "m").unwrap(),
        Merged::Commit { .. }
    ));
    // One that clashes with everything merges onto nothing new alone.
    hub.clash_with_all(&head(5));
    hub.create_branch(REMI, "c", TRUNK).unwrap();
    hub.merge(REMI, "c", &head(5), "m").unwrap();
    assert_eq!(
        hub.merge(REMI, "c", &head(4), "m").unwrap(),
        Merged::Conflict
    );
}

#[test]
fn a_revert_merged_with_trunk_leaves_out_only_what_it_reverts() {
    let hub = hub();
    let after = hub.add(TRUNK, "x");
    let trunk = hub.add(&after, "y");
    hub.set_branch(REMI, "main", &trunk);
    let before = hub.tree(REMI, TRUNK).unwrap();
    let revert = hub.make_commit(REMI, &before, &after, "undo x").unwrap();
    hub.create_branch(REMI, "undo", &revert).unwrap();
    let Merged::Commit { tree, .. } = hub.merge(REMI, "undo", &trunk, "m").unwrap() else {
        panic!("didn't merge");
    };
    assert_eq!(tree, tree_of(&["y"]));
    // Trunk took x out some other way: the revert changes nothing.
    let out = hub.take_out(&trunk, "x");
    hub.set_branch(REMI, "main", &out);
    hub.set_branch(REMI, "undo", &revert);
    let Merged::Commit { tree, .. } = hub.merge(REMI, "undo", &out, "m").unwrap() else {
        panic!("didn't merge");
    };
    assert_eq!(tree, hub.tree(REMI, &out).unwrap());
    // And one that clashes with what came after it conflicts.
    hub.clash("x", "y");
    hub.set_branch(REMI, "undo", &revert);
    assert_eq!(
        hub.merge(REMI, "undo", &trunk, "m").unwrap(),
        Merged::Conflict
    );
}

#[test]
fn a_landing_squashes_links_and_closes_and_refuses_a_moved_head() {
    let hub = hub();
    hub.pr(REMI, 3, &head(3));
    assert_eq!(
        hub.land(REMI, 3, &head(4), &message()).unwrap(),
        PrMerge::Refused("Head branch was modified".into())
    );
    assert_eq!(
        hub.land(REMI, 3, &head(3), &message()).unwrap(),
        PrMerge::Merged
    );
    let trunk = hub.branch_at(REMI, "main").unwrap();
    assert_eq!(hub.parents(&trunk), [TRUNK]);
    // Its squash says the message it was given.
    assert_eq!(
        hub.commit(REMI, &trunk).unwrap().commit.unwrap().message,
        Some(message().whole())
    );
    assert_eq!(hub.holding(&trunk), [head(3)]);
    assert_eq!(hub.commit_pulls(REMI, &trunk).unwrap(), [3]);
    let pr = hub.pull(REMI, 3).unwrap();
    assert!(pr.is_merged() && pr.state == "closed");
    assert_eq!(pr.merge_commit_sha.as_deref(), Some(trunk.as_str()));
    let events: Vec<_> = hub
        .timeline(REMI, 3)
        .unwrap()
        .into_iter()
        .filter_map(|e| e.event)
        .collect();
    assert_eq!(events, ["merged", "closed"]);
    assert_eq!(
        hub.land(REMI, 3, &head(3), &message()).unwrap(),
        PrMerge::Refused("Pull Request is not mergeable".into())
    );
    // GitHub's own merges are its own, a person's commits aren't.
    assert!(!hub.commit(REMI, &head(3)).unwrap().made_by_github());
    hub.create_branch(REMI, "b", TRUNK).unwrap();
    hub.pr(REMI, 4, &head(4));
    let Merged::Commit { sha, .. } = hub.merge(REMI, "b", &head(4), "m").unwrap() else {
        panic!("didn't merge");
    };
    assert!(hub.commit(REMI, &sha).unwrap().made_by_github());
}

#[test]
fn a_failing_call_does_nothing_and_a_lost_answer_hides_what_was_done() {
    let hub = hub();
    hub.pr(REMI, 3, &head(3));
    hub.fail(Call::Land.pr(3));
    assert!(
        hub.land(REMI, 3, &head(3), &message())
            .unwrap_err()
            .unavailable
    );
    assert_eq!(hub.pull(REMI, 3).unwrap().state, "open");
    hub.heal(Call::Land);
    hub.lose(Call::Land);
    assert!(hub.land(REMI, 3, &head(3), &message()).is_err());
    assert!(hub.pull(REMI, 3).unwrap().is_merged());
    // Narrowed to a repo, a status, or a PR, it's only those.
    hub.fail(Call::Mark.repo(REMI).mark(Mark::Red));
    assert!(hub.mark(REMI, &head(3), Mark::Red, "red", None).is_err());
    hub.mark(REMI, &head(3), Mark::Waiting, "waiting", None)
        .unwrap();
    assert_eq!(hub.latest(&head(3)), Some(Mark::Waiting));
    hub.fail(Call::Pull.pr(4));
    assert!(hub.pull(REMI, 3).is_ok());
    assert_eq!(hub.count(Call::Land), 2);
}

#[test]
fn a_stale_read_answers_from_before_and_catches_up() {
    let hub = hub();
    hub.pr(REMI, 3, &head(3));
    hub.stale(Call::Pull);
    hub.land(REMI, 3, &head(3), &message()).unwrap();
    assert!(!hub.pull(REMI, 3).unwrap().is_merged());
    // Another endpoint shows it already.
    assert_eq!(hub.pulls(REMI, Pulls::Open).unwrap().len(), 0);
    hub.fresh(Call::Pull);
    assert!(hub.pull(REMI, 3).unwrap().is_merged());
}

#[test]
fn a_race_changes_github_just_before_its_call_once() {
    let hub = hub();
    hub.pr(REMI, 3, &head(3));
    hub.race(Call::Land.pr(3), |hub| hub.push(REMI, 3, &head(33)));
    assert_eq!(
        hub.land(REMI, 3, &head(3), &message()).unwrap(),
        PrMerge::Refused("Head branch was modified".into())
    );
    assert_eq!(
        hub.land(REMI, 3, &head(33), &message()).unwrap(),
        PrMerge::Merged
    );
}

#[test]
fn answers_come_in_githubs_own_shapes() {
    let hub = hub();
    hub.pr(REMI, 3, &head(3));
    hub.touch(&head(3), &["docs/a.md", "src/b.rs"]);
    let pr = hub.pull(REMI, 3).unwrap();
    assert_eq!((pr.number, pr.head.sha.as_str()), (3, head(3).as_str()));
    assert!(pr.targets_trunk());
    // Statuses newest first, the steward's settling the oldest.
    hub.post_status(&head(3), json!({"context": "gate", "state": "pending"}));
    let statuses = hub.statuses(REMI, &head(3)).unwrap();
    assert_eq!(statuses[0].context, "gate");
    assert_eq!(statuses[1].creator.as_ref().unwrap().login, STEWARD);
    // What a comparison lists, against the merge base.
    let paths = hub.files(REMI, TRUNK, &head(3)).unwrap();
    let listed: Vec<_> = paths.files.iter().map(|t| t.path.as_str()).collect();
    assert_eq!(listed, ["docs/a.md", "src/b.rs"]);
    assert_eq!(
        hub.asked(Call::Files),
        [vec![REMI.to_owned(), TRUNK.into(), head(3)]]
    );
    hub.ci(&head(3), &[("check / check", "success")]);
    let runs = hub.check_runs(REMI, &head(3)).unwrap();
    assert_eq!(runs[0].conclusion.as_deref(), Some("success"));
    assert!(runs[0].check_suite.is_some());
    hub.put_reviews(
        REMI,
        3,
        json!([{"body": "looks fine", "commit_id": head(3)}]),
    );
    assert_eq!(
        hub.reviews(REMI, 3).unwrap()[0].body.as_deref(),
        Some("looks fine")
    );
}

#[test]
fn the_merge_base_is_the_newest_common_ancestor_whichever_way_round() {
    let hub = hub();
    let b = hub.add(TRUNK, "x");
    // A merge whose first parent is the older commit, and a revert of x
    // on b: b is their merge base, however far trunk is from either.
    let m = "a".repeat(40);
    hub.commit_as(&m, &[TRUNK.into(), b.clone()], hub.patches(&b));
    let u = hub.take_out(&b, "x");
    let before = hub.patches(TRUNK);
    let w = hub.world.borrow();
    assert_eq!(w.merge_base(&u, &m), Some(b.clone()));
    assert_eq!(w.merge_base(&m, &u), Some(b));
    // So merging the merge into the revert keeps x out.
    assert_eq!(w.three_way(&u, &m), Some(before));
}

#[test]
fn a_comparison_lists_what_head_took_out_as_well_as_what_it_added() {
    let hub = hub();
    let kept = hub.add(TRUNK, "y");
    let after = hub.add(&kept, "x");
    hub.touch("x", &["CODEOWNERS"]);
    hub.touch("y", &["src/y.rs"]);
    let undo = hub.take_out(&after, "x");
    let paths = hub.files(REMI, &after, &undo).unwrap();
    let listed: Vec<_> = paths.files.iter().map(|t| t.path.as_str()).collect();
    assert_eq!(listed, ["CODEOWNERS"]);
}

mod flow;

mod flow_ci;

mod flow_reader;

mod flow_markers;

mod flow_gates;

mod flow_case;

mod flow_rounds;

mod flow_jobs;

mod flow_renovate;

mod flow_proofs;

mod flow_answers;

mod flow_carried;

mod flow_waits;

mod flow_claims;

mod flow_lost;

mod flow_trains;

mod flow_scope;

mod flow_conflicts;

mod flow_deps;

mod flow_slots;

mod flow_ci_case;
mod flow_legs;

mod flow_docs;

mod flow_findings;

mod flow_obligations;

mod flow_pass;

mod flow_pass_case;

mod flow_compare;

mod flow_pass_review;

mod flow_pass_service;

mod flow_pass_slots;

mod flow_pass_landing;

mod flow_pass_r3;

mod flow_pass_r4;

mod flow_pass_r5;

mod events;
mod events_recovery;

mod events_revision;
