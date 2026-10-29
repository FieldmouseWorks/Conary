//! Landing: a green candidate's PRs merge in order, each through its own
//! PR's merge, onto the trunk the train was built for.

use super::*;

#[test]
fn green_lands_in_order_with_the_trains_status_then_finishes_on_trunks_tree() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3, 4, 6]);
    join_all(&mut store, &hub, &[3, 4]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    // One that joins now waits for the next candidate.
    join_all(&mut store, &hub, &[6]);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.verdicts, [(id, Verdict::Green)]);
    assert_eq!(stepped.landed, [3, 4]);
    // Each head got the train's word before its merge, in order.
    assert_eq!(
        hub.marked(),
        [(head(3), Mark::Landing), (head(4), Mark::Landing)]
    );
    assert_eq!(hub.held("main"), [3, 4].map(head));
    for pr in [3, 4] {
        let c = store.ledger().by_pr(&repo(), pr).unwrap();
        let landing = c.landing.as_ref().unwrap();
        assert_eq!(landing.candidate, Some(id));
        assert_eq!(landing.after.to_string(), hub.merged.borrow()[&pr]);
    }
    assert_eq!(stepped.finished, [id]);
    assert!(store.ledger().candidate(id).unwrap().same_tree());
    // Its branch is gone, and the next candidate is the one that waited.
    let (next, prs) = gating(&stepped);
    assert_eq!(prs, [6]);
    assert_eq!(hub.names(), [train_branch(next), "main".into()]);
    assert_eq!(hub.held(&train_branch(next)), [3, 4, 6].map(head));
}

#[test]
fn trunk_that_moved_meanwhile_isnt_the_candidates_tree() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3, 9]);
    join_all(&mut store, &hub, &[3]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    // Something else reached trunk first.
    let moved = "e".repeat(40);
    hub.holds.borrow_mut().insert(moved.clone(), vec![head(9)]);
    hub.branches.borrow_mut().insert("main".into(), moved);
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    let stepped = step_once(&mut store, &hub);
    assert_eq!((stepped.landed, stepped.finished), (vec![3], vec![id]));
    assert!(!store.ledger().candidate(id).unwrap().same_tree());
}

#[test]
fn a_pr_alone_lands_only_while_trunk_is_its_base_and_is_built_again_once_it_moved() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3, 4, 9]);
    join_all(&mut store, &hub, &[3]);
    assert_eq!(
        join(&mut store, &hub, &config(), &repo(), 4, &head(4), true).unwrap(),
        Joined::Joined
    );
    // The PR ahead of it goes without it.
    let (first, prs) = gating(&step_once(&mut store, &hub));
    assert_eq!(prs, [3]);
    hub.ci(&commit_of(&store, first), &[("check / check", "success")]);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.landed, [3]);
    // Then it's built by itself, on trunk as that landing left it.
    let (id, prs) = gating(&stepped);
    assert_eq!(prs, [4]);
    assert!(store.ledger().candidate(id).unwrap().alone);

    // Something else reached trunk before it landed: its green is for a
    // tree it won't make, so it's dropped and built again on trunk as it is.
    let moved = "e".repeat(40);
    hub.holds
        .borrow_mut()
        .insert(moved.clone(), [3, 9].map(head).to_vec());
    hub.branches
        .borrow_mut()
        .insert("main".into(), moved.clone());
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(
        stepped.verdicts,
        [(id, Verdict::Green), (id, Verdict::Dropped)]
    );
    assert!(stepped.landed.is_empty());
    assert!(!hub.marked().contains(&(head(4), Mark::Landing)));
    let (again, prs) = gating(&stepped);
    assert_eq!(prs, [4]);
    let c = store.ledger().candidate(again).unwrap();
    assert_eq!(c.base.to_string(), moved);

    // On the trunk it was built on, it lands, on its own tree.
    hub.ci(&commit_of(&store, again), &[("check / check", "success")]);
    let stepped = step_once(&mut store, &hub);
    assert_eq!((stepped.landed, stepped.finished), (vec![4], vec![again]));
    assert!(store.ledger().candidate(again).unwrap().same_tree());
}

#[test]
fn a_refused_merge_leaves_and_the_changes_after_it_go_again() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3, 4, 5]);
    hub.refuse.insert(
        4,
        "Required status check \"review-settled\" is expected.".into(),
    );
    join_all(&mut store, &hub, &[3, 4, 5]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.landed, [3]);
    assert_eq!(
        stepped.refused,
        [Refused {
            pr: 4,
            why: "Required status check \"review-settled\" is expected.".into(),
        }]
    );
    assert_eq!(
        store.ledger().candidate(id).unwrap().state,
        State::Broken {
            by: store.ledger().by_pr(&repo(), 4).unwrap().id
        }
    );
    // Its head said it was landing, and says now that it isn't.
    assert_eq!(
        hub.marked(),
        [
            (head(3), Mark::Landing),
            (head(4), Mark::Landing),
            (head(4), Mark::Refused)
        ]
    );
    // 5 is built again, on trunk with 3, without 4.
    let (next, prs) = gating(&stepped);
    assert_eq!(prs, [5]);
    assert_eq!(hub.held(&train_branch(next)), [3, 5].map(head));
    assert_eq!(
        join(&mut store, &hub, &config(), &repo(), 4, &head(4), false).unwrap(),
        Joined::Held(Held::Refused)
    );
    // Once trunk moves, it can try again.
    hub.ci(&commit_of(&store, next), &[("check / check", "success")]);
    hub.refuse.clear();
    assert_eq!(step_once(&mut store, &hub).landed, [5]);
    join_all(&mut store, &hub, &[4]);
}

#[test]
fn a_merge_github_hasnt_shown_yet_waits_for_the_next_step() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3, 4]);
    join_all(&mut store, &hub, &[3, 4]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    hub.lagging.set(true);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.landed, [3]);
    match &stepped.now {
        Now::Landing {
            candidate,
            landings,
        } => {
            assert_eq!(*candidate, id);
            let prs: Vec<u64> = landings.iter().map(|l| l.pr).collect();
            assert_eq!(prs, [3, 4]);
        }
        other => panic!("{other:?}"),
    }
    // Its merge's event: the merge is read, never made again.
    hub.lagging.set(false);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.landed, [4]);
    assert_eq!(stepped.finished, [id]);
    assert_eq!(hub.held("main"), [3, 4].map(head));
}

#[test]
fn a_pr_against_another_branch_never_lands_through_the_train() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new(&[3, 4]);
    // One against another branch isn't joined: its merge would land there.
    hub.targets.borrow_mut().insert(4, "release".into());
    assert_eq!(
        join(&mut store, &hub, &config(), &repo(), 4, &head(4), false).unwrap(),
        Joined::Elsewhere("release".into())
    );
    assert!(joined(&store).is_empty());
    // One retargeted after it joined, its merge base and head the same, is
    // refused before its merge, and its head never says it's landing.
    join_all(&mut store, &hub, &[3]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.targets.borrow_mut().insert(3, "release".into());
    let mut hub = hub;
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    let stepped = step_once(&mut store, &hub);
    assert!(stepped.landed.is_empty() && hub.merged.borrow().is_empty());
    let why = "it targets release, not main".to_owned();
    assert_eq!(stepped.refused, [Refused { pr: 3, why }]);
    assert_eq!(hub.marked(), [(head(3), Mark::Refused)]);
    assert!(joined(&store).is_empty());
}

#[test]
fn a_refused_pr_leaves_the_train_only_once_its_head_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3]);
    hub.refuse
        .insert(3, "Required status check is expected".into());
    join_all(&mut store, &hub, &[3]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    hub.unmarked.set(Some(Mark::Refused));
    assert!(step(&mut store, &hub, &hub, &config(), &repo(), &settings()).is_err());
    assert_eq!(joined(&store), [3]);
    hub.unmarked.set(None);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.refused.len(), 1);
    assert_eq!(hub.marked().last(), Some(&(head(3), Mark::Refused)));
    assert!(joined(&store).is_empty());
}
