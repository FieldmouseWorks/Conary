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
        try_join(&mut store, &hub, 4, &head(4), true).unwrap(),
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
            by: store.ledger().by_pr(&repo(), 4).unwrap().id,
            green: true,
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
        try_join(&mut store, &hub, 4, &head(4), false).unwrap(),
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
        try_join(&mut store, &hub, 4, &head(4), false).unwrap(),
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
fn a_pr_retargeted_as_its_landing_starts_isnt_merged_elsewhere() {
    // Retargeted after the step read it, as its head said it was landing:
    // its base is read again before the merge, which would land on release.
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3]);
    hub.branches
        .borrow_mut()
        .insert("release".into(), TRUNK.into());
    join_all(&mut store, &hub, &[3]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    hub.retarget.replace(Some((3, "release".into())));
    let stepped = step_once(&mut store, &hub);
    assert!(stepped.landed.is_empty() && hub.merged.borrow().is_empty());
    let why = "it targets release, not main".to_owned();
    assert_eq!(stepped.refused, [Refused { pr: 3, why }]);
    assert_eq!(hub.latest(&head(3)), Some(Mark::Refused));
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

#[test]
fn the_train_lands_only_onto_the_default_branch_as_github_says_it_is() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3, 4]);
    hub.targets.borrow_mut().insert(4, "main".into());
    join_all(&mut store, &hub, &[3]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    // The default branch changes while the candidate gates, and its PR is
    // retargeted there with the same merge base and head, so its revision
    // is the same. Built and gated on main, it lands nowhere else.
    let release = "f".repeat(40);
    hub.holds.borrow_mut().insert(release.clone(), Vec::new());
    hub.branches.borrow_mut().insert("release".into(), release);
    hub.default.replace("release".into());
    hub.targets.borrow_mut().insert(3, "release".into());
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    let stepped = step_once(&mut store, &hub);
    assert!(stepped.landed.is_empty() && hub.merged.borrow().is_empty());
    let why = format!("candidate {id} was built on main, not release");
    assert_eq!(stepped.refused, [Refused { pr: 3, why }]);
    // A PR against main doesn't join now.
    assert_eq!(
        try_join(&mut store, &hub, 4, &head(4), false).unwrap(),
        Joined::Elsewhere("main".into())
    );
}

#[test]
fn a_repo_whose_trunk_isnt_main_lands_onto_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3]);
    let main = hub.branches.borrow_mut().remove("main").unwrap();
    hub.branches.borrow_mut().insert("trunk".into(), main);
    hub.default.replace("trunk".into());
    join_all(&mut store, &hub, &[3]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    assert_eq!(store.ledger().candidate(id).unwrap().trunk, "trunk");
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    let stepped = step_once(&mut store, &hub);
    assert_eq!((stepped.landed, stepped.finished), (vec![3], vec![id]));
    assert_eq!(hub.held("trunk"), [head(3)]);
}

/// The train's latest status on a head, with what it said.
fn said(hub: &Hub, sha: &str) -> Option<(Mark, String)> {
    hub.marks
        .borrow()
        .iter()
        .rev()
        .find(|p| p.sha == sha)
        .map(|p| (p.mark, p.description.clone()))
}

#[test]
fn a_heads_landing_success_never_outlives_its_landing() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3, 4]);
    // Joining puts its head's status to pending first: a success from an
    // earlier time in the train lets nothing merge it. Until GitHub takes
    // that, it doesn't join.
    hub.unmarked.set(Some(Mark::Waiting));
    assert!(try_join(&mut store, &hub, 4, &head(4), true).is_err());
    assert!(joined(&store).is_empty());
    hub.unmarked.set(None);
    assert_eq!(
        try_join(&mut store, &hub, 4, &head(4), true).unwrap(),
        Joined::Joined
    );
    assert_eq!(hub.latest(&head(4)), Some(Mark::Waiting));

    // Its merge fails after its head said it was landing, and trunk moves
    // before the next step: the candidate alone is dropped, its head
    // pending first. Until GitHub takes that, the candidate stays.
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    hub.down.set(true);
    assert!(step(&mut store, &hub, &hub, &config(), &repo(), &settings()).is_err());
    assert_eq!(hub.latest(&head(4)), Some(Mark::Landing));
    hub.down.set(false);
    let moved = "e".repeat(40);
    hub.holds.borrow_mut().insert(moved.clone(), vec![head(9)]);
    hub.parents.borrow_mut().insert(moved.clone(), TRUNK.into());
    hub.branches.borrow_mut().insert("main".into(), moved);
    hub.unmarked.set(Some(Mark::Waiting));
    assert!(step(&mut store, &hub, &hub, &config(), &repo(), &settings()).is_err());
    assert_eq!(store.ledger().candidate(id).unwrap().state, State::Green);
    hub.unmarked.set(None);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.verdicts, [(id, Verdict::Dropped)]);
    assert_eq!(hub.latest(&head(4)), Some(Mark::Waiting));
    assert!(stepped.landed.is_empty() && hub.merged.borrow().is_empty());

    // The watchdog dropping a green candidate does the same.
    let (again, _) = gating(&stepped);
    hub.ci(&commit_of(&store, again), &[("check / check", "success")]);
    hub.unmarked.set(Some(Mark::Landing));
    assert!(step(&mut store, &hub, &hub, &config(), &repo(), &settings()).is_err());
    let state = |store: &Store, id| store.ledger().candidate(id).unwrap().state.clone();
    assert_eq!(state(&store, again), State::Green);
    hub.unmarked.set(Some(Mark::Waiting));
    assert!(drop_candidate(&mut store, &hub, &hub, &repo(), again).is_err());
    assert_eq!(state(&store, again), State::Green);
    hub.unmarked.set(None);
    drop_candidate(&mut store, &hub, &hub, &repo(), again).unwrap();
    assert_eq!(state(&store, again), State::Dropped);
    assert_eq!(hub.latest(&head(4)), Some(Mark::Waiting));

    // But not while a landing of it is open: its head says it's landing, so
    // its merge may be on trunk. Nothing in it leaves then either. A step
    // settles it.
    let (third, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, third), &[("check / check", "success")]);
    hub.down.set(true);
    assert!(step(&mut store, &hub, &hub, &config(), &repo(), &settings()).is_err());
    assert!(matches!(
        drop_candidate(&mut store, &hub, &hub, &repo(), third),
        Err(TrainError::Landing(4))
    ));
    assert_eq!(
        leave(&mut store, &hub, &hub, &repo(), 4, Left::Unready).unwrap(),
        Leaving::Landing(4)
    );
    assert_eq!(state(&store, third), State::Green);
    hub.down.set(false);
    assert_eq!(step_once(&mut store, &hub).landed, [4]);

    // Leaving otherwise puts the head to pending first too.
    assert_eq!(
        try_join(&mut store, &hub, 3, &head(3), false).unwrap(),
        Joined::Joined
    );
    hub.unmarked.set(Some(Mark::Waiting));
    assert!(leave(&mut store, &hub, &hub, &repo(), 3, Left::Unready).is_err());
    assert_eq!(joined(&store), [3]);
    hub.unmarked.set(None);
    assert_eq!(
        leave(&mut store, &hub, &hub, &repo(), 3, Left::Unready).unwrap(),
        Leaving::Left
    );
    assert_eq!(
        said(&hub, &head(3)),
        Some((Mark::Waiting, "out of the train".into()))
    );
}

#[test]
fn a_candidate_stopped_while_landing_leaves_no_head_saying_its_landing() {
    // The first of two fails its merge after its head said it was landing;
    // then the second's PR gets a new head, which ingest sees, and that
    // stops the candidate. Before the next is built, the first's head goes
    // to pending: the train isn't landing it.
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3, 5]);
    join_all(&mut store, &hub, &[3, 5]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    hub.down.set(true);
    assert!(step(&mut store, &hub, &hub, &config(), &repo(), &settings()).is_err());
    hub.down.set(false);
    assert_eq!(hub.latest(&head(3)), Some(Mark::Landing));
    // Nothing in it leaves meanwhile, the second no more than the first.
    assert_eq!(
        leave(&mut store, &hub, &hub, &repo(), 5, Left::Unready).unwrap(),
        Leaving::Landing(3)
    );
    hub.pulls.borrow_mut().insert(5, head(55));
    ingest::pull(&mut store, &hub, &config(), &repo(), 5).unwrap();
    assert!(matches!(
        store.ledger().candidate(id).unwrap().state,
        State::Broken { .. }
    ));
    // Until GitHub takes that, nothing new is built.
    hub.unmarked.set(Some(Mark::Waiting));
    assert!(step(&mut store, &hub, &hub, &config(), &repo(), &settings()).is_err());
    assert_eq!(store.ledger().train(&repo()).unwrap().candidates().len(), 1);
    hub.unmarked.set(None);
    let (next, prs) = gating(&step_once(&mut store, &hub));
    assert_ne!(next, id);
    assert_eq!(prs, [3]);
    assert_eq!(
        said(&hub, &head(3)),
        Some((
            Mark::Waiting,
            format!("candidate {id} stopped, so it isn't landing")
        ))
    );
}

#[test]
fn a_merge_github_hasnt_shown_is_waited_for_even_alone() {
    // Alone: GitHub took its merge and doesn't show it yet when the next
    // step comes. Trunk has moved, by that very merge, so the candidate
    // waits rather than being dropped, and lands through it once shown.
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[4]);
    assert_eq!(
        try_join(&mut store, &hub, 4, &head(4), true).unwrap(),
        Joined::Joined
    );
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    hub.lagging.set(true);
    assert_eq!(step_once(&mut store, &hub).landed, [4]);
    let stepped = step_once(&mut store, &hub);
    assert!(stepped.verdicts.is_empty() && stepped.refused.is_empty());
    assert!(matches!(stepped.now, Now::Landing { candidate, .. } if candidate == id));
    hub.lagging.set(false);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.finished, [id]);
    assert!(store.ledger().candidate(id).unwrap().same_tree());
}

#[test]
fn a_merge_whose_answer_was_lost_is_never_made_again() {
    // GitHub's answer to the merge lost, and the merge not shown yet when
    // the next step comes: it waits for GitHub to show it.
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3, 5]);
    join_all(&mut store, &hub, &[3, 5]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    hub.lost.set(true);
    assert!(step(&mut store, &hub, &hub, &config(), &repo(), &settings()).is_err());
    hub.lost.set(false);
    hub.lagging.set(true);
    let stepped = step_once(&mut store, &hub);
    assert!(stepped.landed.is_empty() && stepped.refused.is_empty());
    assert!(matches!(stepped.now, Now::Landing { candidate, .. } if candidate == id));
    hub.lagging.set(false);
    let stepped = step_once(&mut store, &hub);
    assert_eq!((stepped.landed, stepped.finished), (vec![5], vec![id]));
    assert_eq!(hub.held("main"), [3, 5].map(head));
}

#[test]
fn a_landed_heads_success_is_retired_before_the_next_head_says_its_landing() {
    // Once a head has landed, its train status says so, as pending: another
    // PR at that head (opened again after a revert, say) can't merge on it.
    // GitHub not taking that stops the next landing until it does.
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3, 5]);
    join_all(&mut store, &hub, &[3, 5]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    hub.unmarked.set(Some(Mark::Waiting));
    assert!(step(&mut store, &hub, &hub, &config(), &repo(), &settings()).is_err());
    assert_eq!(hub.held("main"), [head(3)]);
    assert_eq!(hub.latest(&head(5)), Some(Mark::Waiting));
    hub.unmarked.set(None);
    let stepped = step_once(&mut store, &hub);
    assert_eq!((stepped.landed, stepped.finished), (vec![5], vec![id]));
    let landed = Some((Mark::Waiting, format!("landed with candidate {id}")));
    assert_eq!(said(&hub, &head(3)), landed);
    assert_eq!(said(&hub, &head(5)), landed);
    // The first was retired before the second said it was landing.
    let at = |sha: &str, mark: Mark| {
        hub.marks
            .borrow()
            .iter()
            .position(|p| p.sha == sha && p.mark == mark)
            .unwrap()
    };
    assert!(at(&head(3), Mark::Waiting) < at(&head(5), Mark::Landing));
}

#[test]
fn a_merge_unseen_when_its_candidate_stopped_is_waited_for_and_lands_through_it() {
    // GitHub took the first's merge and doesn't show it yet when the
    // second's new head stops the candidate. The train builds nothing
    // until the merge shows, and then it lands through the candidate it
    // came through, merged once.
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3, 5, 7]);
    join_all(&mut store, &hub, &[3, 5, 7]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    hub.lagging.set(true);
    assert_eq!(step_once(&mut store, &hub).landed, [3]);
    hub.pulls.borrow_mut().insert(5, head(55));
    ingest::pull(&mut store, &hub, &config(), &repo(), 5).unwrap();
    assert_eq!(
        store.ledger().candidate(id).unwrap().state,
        State::Broken {
            by: store.ledger().by_pr(&repo(), 5).unwrap().id,
            green: true,
        }
    );
    // Only the one landing stays in: the rest may leave.
    assert_eq!(
        leave(&mut store, &hub, &hub, &repo(), 3, Left::Unready).unwrap(),
        Leaving::Landing(3)
    );
    assert_eq!(
        leave(&mut store, &hub, &hub, &repo(), 7, Left::Unready).unwrap(),
        Leaving::Left
    );
    let stepped = step_once(&mut store, &hub);
    assert!(
        matches!(stepped.now, Now::Landing { candidate, ref landings }
        if candidate == id && landings.iter().map(|l| l.pr).collect::<Vec<_>>() == [3])
    );
    assert_eq!(store.ledger().train(&repo()).unwrap().candidates().len(), 1);
    assert_eq!(hub.latest(&head(3)), Some(Mark::Landing));
    hub.lagging.set(false);
    let stepped = step_once(&mut store, &hub);
    assert!(stepped.landed.is_empty() && stepped.now == Now::Idle);
    let three = store.ledger().by_pr(&repo(), 3).unwrap();
    assert_eq!(three.landing.as_ref().unwrap().candidate, Some(id));
    assert_eq!(
        said(&hub, &head(3)),
        Some((Mark::Waiting, format!("landed with candidate {id}")))
    );
    assert_eq!(hub.held("main"), [head(3)]);
}

#[test]
fn a_head_its_stopped_candidate_was_landing_says_so_no_longer_once_its_pr_is_merged_otherwise() {
    // The first's merge failed after its head said it was landing; the
    // second's new head stopped the candidate; then the first was merged
    // at a new head of its own, outside the candidate: settled there, a
    // landing that isn't the candidate's, and otherwise not a landing at
    // all. Either way its old head isn't left saying it's landing, and
    // nothing waits on it.
    for settled in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(&dir);
        let mut hub = Hub::new(&[3, 5]);
        if settled {
            let theirs = hub.statuses[&head(3)].clone();
            hub.statuses.insert(head(33), theirs);
        }
        join_all(&mut store, &hub, &[3, 5]);
        let (id, _) = gating(&step_once(&mut store, &hub));
        hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
        hub.down.set(true);
        assert!(step(&mut store, &hub, &hub, &config(), &repo(), &settings()).is_err());
        hub.down.set(false);
        hub.pulls.borrow_mut().insert(5, head(55));
        ingest::pull(&mut store, &hub, &config(), &repo(), 5).unwrap();
        hub.pulls.borrow_mut().insert(3, head(33));
        assert_eq!(
            hub.land("FieldmouseWorks/remi", 3, &head(33)).unwrap(),
            PrMerge::Merged
        );
        assert_eq!(step_once(&mut store, &hub).now, Now::Idle);
        let three = store.ledger().by_pr(&repo(), 3).unwrap();
        assert_eq!(
            three.landing.as_ref().map(|l| l.candidate),
            settled.then_some(None)
        );
        let says = if settled {
            format!("landed, though not with candidate {id}")
        } else {
            format!("closed, so candidate {id} isn't landing it")
        };
        assert_eq!(said(&hub, &head(3)), Some((Mark::Waiting, says)));
        assert_eq!(hub.held("main"), [head(33)]);
    }
}

/// The steward's word on a head, newest first, then the success it said
/// when the PR joined.
fn then_said(hub: &mut Hub, pr: u64, state: &str, by: &str) {
    hub.statuses.insert(
        head(pr),
        json!([{"context": "review-settled", "state": state, "target_url": null,
                "created_at": "2026-10-29T12:00:00Z", "creator": {"login": by, "type": "Bot"}},
               {"context": "review-settled", "state": "success", "target_url": null,
                "created_at": "2026-10-29T10:00:00Z", "creator": {"login": STEWARD, "type": "Bot"}}]),
    );
}

#[test]
fn a_pr_the_steward_no_longer_settles_leaves_instead_of_landing_whatever_the_rules() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3, 4, 6]);
    join_all(&mut store, &hub, &[3, 4, 6]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    // Held once its candidate was green: GitHub would merge it all the same.
    then_said(&mut hub, 4, "pending", STEWARD);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(
        (&stepped.landed[..], &stepped.unready[..]),
        (&[3][..], &[4][..])
    );
    assert_eq!(hub.held("main"), [head(3)]);
    assert_eq!(hub.latest(&head(4)), Some(Mark::Waiting));
    assert!(!hub.marked().contains(&(head(4), Mark::Landing)));
    // The one after it waits again, and is built without it.
    assert_eq!(joined(&store), [6]);
    assert_eq!(gating(&stepped).1, [6]);
}

#[test]
fn only_the_stewards_own_word_takes_a_pr_out() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let mut hub = Hub::new(&[3, 4]);
    join_all(&mut store, &hub, &[3, 4]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    // Anyone who can push can post the steward's context.
    then_said(&mut hub, 4, "pending", "someone");
    let stepped = step_once(&mut store, &hub);
    assert_eq!((stepped.landed, stepped.unready), (vec![3, 4], vec![]));
}
