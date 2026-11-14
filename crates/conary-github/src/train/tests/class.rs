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
    let hub = github(&[3, 4, 5]);
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
    let hub = github(&[3, 4]);
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
        &[],
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
    let hub = github(&[3, 4, 5, 6]);
    join_all(&mut store, &hub, &[3, 4, 5, 6]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    // GitHub takes 3's merge and doesn't show it merged yet.
    hub.stale(Call::Pull.pr(3));
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
    hub.fresh(Call::Pull);
    let stepped = step_once(&mut store, &hub);
    // 4 instead of its merge; then 5 and 6 wait again, and 6 goes back in
    // alone before it's built with 5.
    assert_eq!(stepped.reseated, [4, 6]);
    assert!(stepped.landed.is_empty());
    assert_eq!(hub.latest(&head(4)), Some(Mark::Waiting));
    assert!(!hub.merged(REMI, 4).is_some());
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
    let hub = github(&[3, 4]);
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
    hub.fail(unmarked(Mark::Waiting));
    assert!(step(&mut store, &hub, &hub, &config(), &repo(), &settings()).is_err());
    assert_eq!(joined(&store), [3, 4]);
    assert_eq!(
        store.ledger().train(&repo()).unwrap().in_flight(),
        Some(first)
    );
    hub.heal(Call::Mark);
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
    let hub = github(&[3, 4]);
    join_all(&mut store, &hub, &[3, 4]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    hub.fail(Call::Land);
    assert!(step(&mut store, &hub, &hub, &config(), &repo(), &settings()).is_err());
    assert_eq!(hub.latest(&head(3)), Some(Mark::Landing));
    hub.heal(Call::Land);
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
    assert!(stepped.landed.is_empty() && hub.landed(REMI).is_empty());
    assert_eq!(hub.latest(&head(3)), Some(Mark::Waiting));
    assert_eq!(gating(&stepped).1, [4]);
    assert!(alone(&store, 3));
}

/// The steward's `cn class` of a PR at its head, labels and all: its class,
/// and the raises it settles the PR with.
fn classed(store: &mut Store, hub: &Hub, pr: u64) -> (Class, Vec<String>) {
    let got =
        crate::class::at_once(store, hub, &config(), &policy(), &repo(), pr, &head(pr)).unwrap();
    (got.class(), got.said.raised)
}

/// The steward's join, of a PR it settled with `settled`.
fn join_one(store: &mut Store, hub: &Hub, pr: u64, settled: &[String]) -> Joined {
    join(
        store,
        hub,
        hub,
        &config(),
        &repo(),
        pr,
        &head(pr),
        settled,
        &policy(),
        None,
    )
    .unwrap()
}

/// Its author labelled it `r2`: a PR whose paths are R0 and that the
/// steward settled with the label joins alone. Settled without it, it's
/// held out, the join's own class recording the label notwithstanding,
/// and asked again it's held out again.
#[test]
fn a_pr_labelled_r2_rides_alone_once_the_steward_settled_it_so() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[3, 5]);
    join_all(&mut store, &hub, &[3]);
    hub.label(REMI, 5, &["R2"]);
    assert_eq!(join_one(&mut store, &hub, 5, &[]), Joined::Raised);
    assert_eq!(join_one(&mut store, &hub, 5, &[]), Joined::Raised);
    assert_eq!(joined(&store), [3]);
    let (class, raised) = classed(&mut store, &hub, 5);
    assert_eq!((class, &raised[..]), (Class::R2, &["r2".to_owned()][..]));
    assert_eq!(join_one(&mut store, &hub, 5, &raised), Joined::Joined);
    assert!(alone(&store, 5) && !alone(&store, 3));
}

/// The steward classed a PR with no label, and settled it so; then it's
/// labelled and someone else's `cn class` records the label before the
/// steward's join. The join goes by what the steward settled, not by what
/// the ledger holds by then: held out.
#[test]
fn a_label_recorded_by_another_class_between_the_stewards_and_its_join_holds_it_out() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[3]);
    let (_, settled) = classed(&mut store, &hub, 3);
    assert!(settled.is_empty());
    hub.label(REMI, 3, &["review"]);
    classed(&mut store, &hub, 3);
    assert_eq!(join_one(&mut store, &hub, 3, &settled), Joined::Raised);
    assert!(joined(&store).is_empty());
}

/// Labelled to raise it once it's in, and classed with the label: the
/// next step takes it out rather than putting it back alone, whichever
/// label, since the steward settled it without the label. Taking the label
/// off lowers nothing: it joins again only settled with the raise.
#[test]
fn a_pr_classed_with_a_label_since_it_joined_leaves_the_train() {
    for (label, class) in [("r2", Class::R2), ("review", Class::R0)] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(&dir);
        let hub = github(&[3, 4]);
        join_all(&mut store, &hub, &[3, 4]);
        hub.label(REMI, 4, &[label]);
        let (got, raised) = classed(&mut store, &hub, 4);
        assert_eq!(got, class, "{label}");
        let stepped = step_once(&mut store, &hub);
        assert_eq!(
            (&stepped.unready[..], &stepped.reseated[..]),
            (&[4][..], &[][..]),
            "{label}"
        );
        assert_eq!(hub.latest(&head(4)), Some(Mark::Waiting));
        hub.label(REMI, 4, &[]);
        assert_eq!(
            join_one(&mut store, &hub, 4, &[]),
            Joined::Raised,
            "{label}"
        );
        assert_eq!(classed(&mut store, &hub, 4).1, raised, "{label}");
        assert_eq!(
            join_one(&mut store, &hub, 4, &raised),
            Joined::Joined,
            "{label}"
        );
        assert_eq!(alone(&store, 4), label == "r2");
    }
}

/// Labelled to raise it while its candidate gated, with no class asked for
/// since: the step that would merge it takes it out instead, whichever
/// label, and the ones after it are built again without it.
#[test]
fn a_pr_labelled_since_it_joined_leaves_instead_of_landing() {
    for label in ["r2", "Review"] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(&dir);
        let hub = github(&[3, 4, 6]);
        join_all(&mut store, &hub, &[3, 4, 6]);
        let (id, _) = gating(&step_once(&mut store, &hub));
        hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
        hub.label(REMI, 4, &["bug", label]);
        let stepped = step_once(&mut store, &hub);
        assert_eq!(
            (&stepped.landed[..], &stepped.unready[..]),
            (&[3][..], &[4][..]),
            "{label}"
        );
        assert_eq!(hub.held(REMI, "main"), [head(3)]);
        assert_eq!(hub.latest(&head(4)), Some(Mark::Waiting));
        assert_eq!(joined(&store), [6]);
    }
}

/// Classed with a label since it joined, its candidate green and nothing
/// of it landed yet: it's out before anything lands, and the rest are
/// built again without it.
#[test]
fn a_pr_classed_with_a_label_in_a_green_candidate_leaves_before_it_lands() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[3, 4, 6]);
    join_all(&mut store, &hub, &[3, 4, 6]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    hub.label(REMI, 4, &["review"]);
    classed(&mut store, &hub, 4);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(
        (&stepped.landed[..], &stepped.unready[..]),
        (&[][..], &[4][..])
    );
    assert!(hub.held(REMI, "main").is_empty());
    assert_eq!(hub.latest(&head(4)), Some(Mark::Waiting));
    let (next, prs) = gating(&stepped);
    assert_ne!(next, id);
    assert_eq!(prs, [3, 6]);
}

/// Labelled while its head says it's landing, after the read that let it:
/// the last read before the merge sees the label, and it leaves instead,
/// its head saying it's landing no more.
#[test]
fn a_pr_labelled_as_it_lands_leaves_instead_of_merging() {
    for label in ["r2", "review"] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(&dir);
        let hub = github(&[3]);
        join_all(&mut store, &hub, &[3]);
        let (id, _) = gating(&step_once(&mut store, &hub));
        hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
        hub.race(landing(), move |h| h.label(REMI, 3, &[label]));
        let stepped = step_once(&mut store, &hub);
        assert_eq!(
            (&stepped.landed[..], &stepped.unready[..]),
            (&[][..], &[3][..]),
            "{label}"
        );
        assert!(hub.held(REMI, "main").is_empty());
        assert_eq!(hub.latest(&head(3)), Some(Mark::Waiting));
        assert!(joined(&store).is_empty());
    }
}

/// A label already counted when it joined takes nothing out.
#[test]
fn a_label_it_joined_with_lands_it_as_classed() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[3]);
    hub.label(REMI, 3, &["review"]);
    let (_, raised) = classed(&mut store, &hub, 3);
    assert_eq!(join_one(&mut store, &hub, 3, &raised), Joined::Joined);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    let stepped = step_once(&mut store, &hub);
    assert_eq!((stepped.landed, stepped.unready), (vec![3], vec![]));
}

/// Classed with a label while its own landing is open (its head said it
/// was landing, and the merge didn't go), the label off again before the
/// next step: what the ledger recorded takes it out instead of its merge,
/// though the open landing kept the step's reseat from it.
#[test]
fn a_pr_classed_with_a_label_while_its_landing_is_open_leaves_before_its_merge() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[3]);
    join_all(&mut store, &hub, &[3]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    hub.fail(Call::Land);
    assert!(step(&mut store, &hub, &hub, &config(), &repo(), &settings()).is_err());
    assert_eq!(hub.latest(&head(3)), Some(Mark::Landing));
    hub.heal(Call::Land);
    hub.label(REMI, 3, &["review"]);
    classed(&mut store, &hub, 3);
    hub.label(REMI, 3, &[]);
    let stepped = step_once(&mut store, &hub);
    assert!(stepped.landed.is_empty());
    assert_eq!(stepped.unready, [3]);
    assert_eq!(hub.latest(&head(3)), Some(Mark::Waiting));
    assert!(!hub.merged(REMI, 3).is_some());
    assert!(joined(&store).is_empty());
}
