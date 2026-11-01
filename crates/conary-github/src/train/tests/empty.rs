//! Empty merges: a PR whose merge, after trunk and the PRs ahead of it in a
//! candidate, would change nothing leaves the train and is never merged.

use super::*;

#[test]
fn a_head_that_changes_nothing_after_the_ones_ahead_leaves_and_is_never_merged() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3, 4, 5]);
    // 4 is the same fix as 3: merged after it, it changes nothing.
    hub.same.insert(head(4), head(3));
    join_all(&mut store, &hub, &[3, 4, 5]);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(
        stepped.empty,
        [Empty {
            pr: 4,
            onto: vec![3],
            base: TRUNK.parse().unwrap(),
        }]
    );
    assert!(stepped.conflicts.is_empty());
    let (id, prs) = gating(&stepped);
    assert_eq!(prs, [3, 5]);
    // Only the candidate's own branch is left.
    assert_eq!(hub.names(), [train_branch(id), "main".into()]);
    assert_eq!(joined(&store), [3, 5]);
    // Held while 3 is on its way to trunk ahead of it.
    assert_eq!(
        try_join(&mut store, &hub, 4, &head(4), false).unwrap(),
        Joined::Held(Held::Empty)
    );
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    assert_eq!(step_once(&mut store, &hub).landed, [3, 5]);
    // Trunk moved: it goes again, and changes nothing on trunk itself now,
    // which has what it does.
    join_all(&mut store, &hub, &[4]);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(
        stepped.empty,
        [Empty {
            pr: 4,
            onto: vec![],
            base: hub.branch_at("main").unwrap().parse().unwrap(),
        }]
    );
    assert_eq!(stepped.now, Now::Idle);
    assert!(joined(&store).is_empty());
    assert!(!hub.merged.borrow().contains_key(&4));
}

#[test]
fn a_head_already_in_a_pr_ahead_of_it_leaves_as_one_that_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3, 4]);
    // 3's branch was made on 4's: merging 3 brings 4's head, so 4's merge
    // after it has nothing left to bring (GitHub's 204).
    hub.stacked.insert(head(3), head(4));
    join_all(&mut store, &hub, &[3, 4]);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(
        stepped.empty,
        [Empty {
            pr: 4,
            onto: vec![3],
            base: TRUNK.parse().unwrap(),
        }]
    );
    let (id, prs) = gating(&stepped);
    assert_eq!(prs, [3]);
    assert_eq!(
        try_join(&mut store, &hub, 4, &head(4), false).unwrap(),
        Joined::Held(Held::Empty)
    );
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    assert_eq!(step_once(&mut store, &hub).landed, [3]);
    // 3's squash brought what 4 does to trunk: joined again, 4 still
    // changes nothing, and is never merged.
    join_all(&mut store, &hub, &[4]);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(
        stepped.empty,
        [Empty {
            pr: 4,
            onto: vec![],
            base: hub.branch_at("main").unwrap().parse().unwrap(),
        }]
    );
    assert!(joined(&store).is_empty());
    assert!(!hub.merged.borrow().contains_key(&4));
}
