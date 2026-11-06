//! A PR's class in the train: one classed R2 after it joined, under a new
//! policy, goes back in alone, out of any candidate gating it with others.

use super::*;

/// The machine's policy changed: a new digest, so each revision is read
/// again, and a PR touching a secret is still R2.
fn new_policy() -> Policy {
    Policy::parse(br#"{"r2": ["^secrets/"], "docs": ["^docs/"]}"#).unwrap()
}

fn alone(store: &Store, pr: u64) -> bool {
    let ledger = store.ledger();
    let c = ledger.by_pr(&repo(), pr).unwrap();
    ledger.train(&repo()).unwrap().alone(c.id)
}

/// The steward asks a seated PR's class each pass (`cn class`); the next
/// step puts it back alone.
#[test]
fn a_pr_classed_r2_after_it_joined_leaves_its_candidate_and_rides_alone() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3, 4, 5]);
    join_all(&mut store, &hub, &[3, 4, 5]);
    let (first, prs) = gating(&step_once(&mut store, &hub));
    assert_eq!(prs, [3, 4, 5]);
    r2(&hub, 4);
    let c = store.ledger().by_pr(&repo(), 4).unwrap();
    let (change, revision) = (c.id, c.current.unwrap());
    crate::class::of(&mut store, &hub, &new_policy(), &repo(), change, revision).unwrap();
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.reseated, [4]);
    let (second, prs) = gating(&stepped);
    assert_ne!(second, first);
    assert_eq!(prs, [3, 5]);
    assert_eq!(joined(&store), [3, 5, 4]);
    assert!(alone(&store, 4) && !alone(&store, 3));
    assert_eq!(hub.latest(&head(4)), Some(Mark::Waiting));
    // Nothing more to reseat.
    hub.ci(&commit_of(&store, second), &[("check / check", "success")]);
    let stepped = step_once(&mut store, &hub);
    assert!(stepped.reseated.is_empty());
    assert_eq!(stepped.landed, [3, 5]);
    let (_, prs) = gating(&stepped);
    assert_eq!(prs, [4]);
}

/// Asked to join again at a head it's in at, under a policy that makes it
/// R2 now, it goes back in alone there and then.
#[test]
fn a_seated_pr_joined_again_under_a_policy_that_makes_it_r2_rides_alone() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new(&[3, 4]);
    join_all(&mut store, &hub, &[3, 4]);
    let (_, prs) = gating(&step_once(&mut store, &hub));
    assert_eq!(prs, [3, 4]);
    r2(&hub, 3);
    let again = join(
        &mut store,
        &hub,
        &hub,
        &config(),
        &repo(),
        3,
        &head(3),
        &new_policy(),
        None,
    );
    assert_eq!(again.unwrap(), Joined::Already);
    assert_eq!(joined(&store), [4, 3]);
    assert!(alone(&store, 3));
    assert!(store.ledger().train(&repo()).unwrap().in_flight().is_none());
    let (_, prs) = gating(&step_once(&mut store, &hub));
    assert_eq!(prs, [4]);
}

/// One whose green candidate's landing is open stays: that candidate's gate
/// ran on its exact tree.
#[test]
fn a_pr_classed_r2_while_its_green_candidate_lands_stays_in_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3, 4]);
    join_all(&mut store, &hub, &[3, 4]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    // GitHub takes 3's merge and doesn't show it merged yet.
    hub.lagging.set(true);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.landed, [3]);
    assert!(matches!(stepped.now, Now::Landing { .. }));
    r2(&hub, 4);
    let c = store.ledger().by_pr(&repo(), 4).unwrap();
    let (change, revision) = (c.id, c.current.unwrap());
    crate::class::of(&mut store, &hub, &new_policy(), &repo(), change, revision).unwrap();
    hub.lagging.set(false);
    let stepped = step_once(&mut store, &hub);
    assert!(stepped.reseated.is_empty());
    assert_eq!(stepped.landed, [4]);
    assert_eq!(stepped.finished, [id]);
}
