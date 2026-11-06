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
    crate::class::of(
        &mut store,
        &hub,
        &new_policy(),
        &repo(),
        change,
        revision,
        Vec::new(),
    )
    .unwrap();
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

/// Classed R2 while its green candidate is landing: the merge ahead of it
/// that GitHub hasn't shown yet is waited for, never undone, and then it
/// goes back in alone rather than land on a tree no gate ran, wherever it is
/// in the candidate. The ones after it wait again.
#[test]
fn a_pr_classed_r2_while_its_green_candidate_lands_is_reseated_before_its_merge() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3, 4, 5, 6]);
    join_all(&mut store, &hub, &[3, 4, 5, 6]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    // GitHub takes 3's merge and doesn't show it merged yet.
    hub.lagging.set(true);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.landed, [3]);
    assert!(matches!(stepped.now, Now::Landing { .. }));
    for pr in [4, 6] {
        r2(&hub, pr);
        let c = store.ledger().by_pr(&repo(), pr).unwrap();
        let (change, revision) = (c.id, c.current.unwrap());
        crate::class::of(
            &mut store,
            &hub,
            &new_policy(),
            &repo(),
            change,
            revision,
            Vec::new(),
        )
        .unwrap();
    }
    hub.lagging.set(false);
    let stepped = step_once(&mut store, &hub);
    // 4 instead of its merge; then 5 and 6 wait again, and 6 goes back in
    // alone before it's built with 5.
    assert_eq!(stepped.reseated, [4, 6]);
    assert!(stepped.landed.is_empty());
    assert_eq!(hub.latest(&head(4)), Some(Mark::Waiting));
    assert!(!hub.merged.borrow().contains_key(&4));
    let (next, prs) = gating(&stepped);
    assert_ne!(next, id);
    assert_eq!(prs, [5]);
    assert_eq!(joined(&store), [5, 4, 6]);
    assert!(alone(&store, 4) && alone(&store, 6) && !alone(&store, 5));
}

/// GitHub not taking the reseat's status stops the step before anything is
/// recorded: the PR is still in, and the next step reseats it.
#[test]
fn a_reseat_github_wont_mark_leaves_the_pr_in_for_the_next_step() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new(&[3, 4]);
    join_all(&mut store, &hub, &[3, 4]);
    let (first, _) = gating(&step_once(&mut store, &hub));
    r2(&hub, 4);
    let c = store.ledger().by_pr(&repo(), 4).unwrap();
    let (change, revision) = (c.id, c.current.unwrap());
    crate::class::of(
        &mut store,
        &hub,
        &new_policy(),
        &repo(),
        change,
        revision,
        Vec::new(),
    )
    .unwrap();
    hub.unmarked.set(Some(Mark::Waiting));
    assert!(step(&mut store, &hub, &hub, &config(), &repo(), &settings()).is_err());
    assert_eq!(joined(&store), [3, 4]);
    assert_eq!(
        store.ledger().train(&repo()).unwrap().in_flight(),
        Some(first)
    );
    hub.unmarked.set(None);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.reseated, [4]);
    assert_eq!(joined(&store), [3, 4]);
    assert!(alone(&store, 4));
}

/// A landing open when it's classed R2, its head saying it's landing and its
/// merge failed: nothing may take it out while its merge may be on trunk,
/// so the step that finds it not merged reseats it then, instead of merging.
#[test]
fn a_pr_classed_r2_while_its_landing_is_open_is_reseated_once_its_not_merged() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3, 4]);
    join_all(&mut store, &hub, &[3, 4]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    hub.down.set(true);
    assert!(step(&mut store, &hub, &hub, &config(), &repo(), &settings()).is_err());
    assert_eq!(hub.latest(&head(3)), Some(Mark::Landing));
    hub.down.set(false);
    r2(&hub, 3);
    let c = store.ledger().by_pr(&repo(), 3).unwrap();
    let (change, revision) = (c.id, c.current.unwrap());
    crate::class::of(
        &mut store,
        &hub,
        &new_policy(),
        &repo(),
        change,
        revision,
        Vec::new(),
    )
    .unwrap();
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.reseated, [3]);
    assert!(stepped.landed.is_empty() && hub.merged.borrow().is_empty());
    assert_eq!(hub.latest(&head(3)), Some(Mark::Waiting));
    assert_eq!(gating(&stepped).1, [4]);
    assert!(alone(&store, 3));
}

/// Its author labelled it `r2`: a PR whose paths are R0 joins alone, and
/// one already seated goes back in alone at the next step, once its class
/// is asked again with the label.
#[test]
fn a_pr_labelled_r2_rides_alone() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new(&[3, 4, 5]);
    hub.labels.borrow_mut().insert(5, vec!["R2"]);
    join_all(&mut store, &hub, &[3, 4, 5]);
    assert!(alone(&store, 5) && !alone(&store, 3));
    let c = store.ledger().by_pr(&repo(), 4).unwrap();
    let (change, revision) = (c.id, c.current.unwrap());
    let got = crate::class::of(
        &mut store,
        &hub,
        &policy(),
        &repo(),
        change,
        revision,
        vec!["r2".into()],
    )
    .unwrap();
    assert_eq!(got.said.because.as_deref(), Some("labelled r2"));
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.reseated, [4]);
    assert!(alone(&store, 4));
}

/// Labelled to raise it while its candidate gated, with no class asked for
/// since: the step that would merge it takes it out instead, whichever
/// label, and the ones after it are built again without it.
#[test]
fn a_pr_labelled_since_it_joined_leaves_instead_of_landing() {
    for label in ["r2", "Review"] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(&dir);
        let mut hub = Hub::new(&[3, 4, 6]);
        join_all(&mut store, &hub, &[3, 4, 6]);
        let (id, _) = gating(&step_once(&mut store, &hub));
        hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
        hub.labels.borrow_mut().insert(4, vec!["bug", label]);
        let stepped = step_once(&mut store, &hub);
        assert_eq!(
            (&stepped.landed[..], &stepped.unready[..]),
            (&[3][..], &[4][..]),
            "{label}"
        );
        assert_eq!(hub.held("main"), [head(3)]);
        assert_eq!(hub.latest(&head(4)), Some(Mark::Waiting));
        assert_eq!(joined(&store), [6]);
    }
}

/// A label already counted when it joined takes nothing out.
#[test]
fn a_label_it_joined_with_lands_it_as_classed() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3]);
    hub.labels.borrow_mut().insert(3, vec!["review"]);
    join_all(&mut store, &hub, &[3]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    let stepped = step_once(&mut store, &hub);
    assert_eq!((stepped.landed, stepped.unready), (vec![3], vec![]));
}
