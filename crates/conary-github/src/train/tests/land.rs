//! Landing: a green candidate's PRs merge in order, each through its own
//! PR's merge, onto the trunk the train was built for.

use super::*;

#[test]
fn green_lands_in_order_with_the_trains_status_then_finishes_on_trunks_tree() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[3, 4, 6]);
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
    assert_eq!(hub.held(REMI, "main"), [3, 4].map(head));
    for pr in [3, 4] {
        let c = store.ledger().by_pr(&repo(), pr).unwrap();
        let landing = c.landing.as_ref().unwrap();
        assert_eq!(landing.candidate, Some(id));
        assert_eq!(landing.after.to_string(), hub.merged(REMI, pr).unwrap());
    }
    assert_eq!(stepped.finished, [id]);
    assert!(store.ledger().candidate(id).unwrap().same_tree());
    // Its branch is gone, and the next candidate is the one that waited.
    let (next, prs) = gating(&stepped);
    assert_eq!(prs, [6]);
    assert_eq!(hub.names(REMI), [train_branch(next), "main".into()]);
    assert_eq!(hub.held(REMI, &train_branch(next)), [3, 4, 6].map(head));
}

#[test]
fn trunk_that_moved_meanwhile_isnt_the_candidates_tree() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[3, 9]);
    join_all(&mut store, &hub, &[3]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    // Something else reached trunk first.
    let moved = "e".repeat(40);
    hub.change(&moved, TRUNK, &head(9));
    hub.set_branch(REMI, "main", &moved);
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    let stepped = step_once(&mut store, &hub);
    assert_eq!((stepped.landed, stepped.finished), (vec![3], vec![id]));
    assert!(!store.ledger().candidate(id).unwrap().same_tree());
}

#[test]
fn a_pr_alone_lands_only_while_trunk_is_its_base_and_is_built_again_once_it_moved() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[3, 4, 9]);
    r2(&hub, 4);
    join_all(&mut store, &hub, &[3]);
    assert_eq!(
        try_join(&mut store, &hub, 4, &head(4)).unwrap(),
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
    let trunk = hub.branch_at(REMI, "main").unwrap();
    hub.change(&moved, &trunk, &head(9));
    hub.set_branch(REMI, "main", &moved);
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
    let hub = github(&[3, 4, 5]);
    hub.refuse(
        REMI,
        4,
        "Required status check \"review-settled\" is expected.",
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
    assert_eq!(hub.held(REMI, &train_branch(next)), [3, 5].map(head));
    assert_eq!(
        try_join(&mut store, &hub, 4, &head(4)).unwrap(),
        Joined::Held(Held::Refused)
    );
    // Once trunk moves, it can try again.
    hub.ci(&commit_of(&store, next), &[("check / check", "success")]);
    hub.unrefuse();
    assert_eq!(step_once(&mut store, &hub).landed, [5]);
    join_all(&mut store, &hub, &[4]);
}

#[test]
fn a_merge_github_hasnt_shown_yet_waits_for_the_next_step() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[3, 4]);
    join_all(&mut store, &hub, &[3, 4]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    hub.stale(Call::Pull.pr(3));
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
    hub.fresh(Call::Pull);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.landed, [4]);
    assert_eq!(stepped.finished, [id]);
    assert_eq!(hub.held(REMI, "main"), [3, 4].map(head));
}

#[test]
fn a_pr_against_another_branch_never_lands_through_the_train() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[3, 4]);
    // One against another branch isn't joined: its merge would land there.
    hub.retarget(REMI, 4, "release");
    assert_eq!(
        try_join(&mut store, &hub, 4, &head(4)).unwrap(),
        Joined::Elsewhere("release".into())
    );
    assert!(joined(&store).is_empty());
    // One retargeted after it joined, its merge base and head the same, is
    // refused before its merge, and its head never says it's landing.
    join_all(&mut store, &hub, &[3]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.retarget(REMI, 3, "release");
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    let stepped = step_once(&mut store, &hub);
    assert!(stepped.landed.is_empty() && hub.landed(REMI).is_empty());
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
    let hub = github(&[3]);
    hub.set_branch(REMI, "release", TRUNK);
    join_all(&mut store, &hub, &[3]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    hub.race(landing(), |h| h.retarget(REMI, 3, "release"));
    let stepped = step_once(&mut store, &hub);
    assert!(stepped.landed.is_empty() && hub.landed(REMI).is_empty());
    let why = "it targets release, not main".to_owned();
    assert_eq!(stepped.refused, [Refused { pr: 3, why }]);
    assert_eq!(hub.latest(&head(3)), Some(Mark::Refused));
}

#[test]
fn a_refused_pr_leaves_the_train_only_once_its_head_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[3]);
    hub.refuse(REMI, 3, "Required status check is expected");
    join_all(&mut store, &hub, &[3]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    hub.fail(unmarked(Mark::Refused));
    assert!(step(&mut store, &hub, &hub, &config(), &repo(), &settings()).is_err());
    assert_eq!(joined(&store), [3]);
    hub.heal(Call::Mark);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.refused.len(), 1);
    assert_eq!(hub.marked().last(), Some(&(head(3), Mark::Refused)));
    assert!(joined(&store).is_empty());
}

#[test]
fn the_train_lands_only_onto_the_default_branch_as_github_says_it_is() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[3, 4]);
    hub.retarget(REMI, 4, "main");
    join_all(&mut store, &hub, &[3]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    // The default branch changes while the candidate gates, and its PR is
    // retargeted there with the same merge base and head, so its revision
    // is the same. Built and gated on main, it lands nowhere else.
    let release = "f".repeat(40);
    hub.change(&release, TRUNK, "release");
    hub.set_branch(REMI, "release", &release);
    hub.set_default(REMI, "release");
    hub.retarget(REMI, 3, "release");
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    let stepped = step_once(&mut store, &hub);
    assert!(stepped.landed.is_empty() && hub.landed(REMI).is_empty());
    let why = format!("candidate {id} was built on main, not release");
    assert_eq!(stepped.refused, [Refused { pr: 3, why }]);
    // A PR against main doesn't join now.
    assert_eq!(
        try_join(&mut store, &hub, 4, &head(4)).unwrap(),
        Joined::Elsewhere("main".into())
    );
}

#[test]
fn a_repo_whose_trunk_isnt_main_lands_onto_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[3]);
    hub.rename_branch(REMI, "main", "trunk");
    join_all(&mut store, &hub, &[3]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    assert_eq!(store.ledger().candidate(id).unwrap().trunk, "trunk");
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    let stepped = step_once(&mut store, &hub);
    assert_eq!((stepped.landed, stepped.finished), (vec![3], vec![id]));
    assert_eq!(hub.held(REMI, "trunk"), [head(3)]);
}

/// The train's latest status on a head, with what it said.
fn said(hub: &Hub, sha: &str) -> Option<(Mark, String)> {
    hub.marks()
        .iter()
        .rev()
        .find(|p| p.sha == sha)
        .map(|p| (p.mark, p.description.clone()))
}

#[test]
fn a_heads_landing_success_never_outlives_its_landing() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[3, 4]);
    r2(&hub, 4);
    // Joining puts its head's status to pending first: a success from an
    // earlier time in the train lets nothing merge it. Until GitHub takes
    // that, it doesn't join.
    hub.fail(unmarked(Mark::Waiting));
    assert!(try_join(&mut store, &hub, 4, &head(4)).is_err());
    assert!(joined(&store).is_empty());
    hub.heal(Call::Mark);
    assert_eq!(
        try_join(&mut store, &hub, 4, &head(4)).unwrap(),
        Joined::Joined
    );
    assert_eq!(hub.latest(&head(4)), Some(Mark::Waiting));

    // Its merge fails after its head said it was landing, and trunk moves
    // before the next step: the candidate alone is dropped, its head
    // pending first. Until GitHub takes that, the candidate stays.
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    hub.fail(Call::Land);
    assert!(step(&mut store, &hub, &hub, &config(), &repo(), &settings()).is_err());
    assert_eq!(hub.latest(&head(4)), Some(Mark::Landing));
    hub.heal(Call::Land);
    let moved = "e".repeat(40);
    hub.change(&moved, TRUNK, &head(9));
    hub.set_branch(REMI, "main", &moved);
    hub.fail(unmarked(Mark::Waiting));
    assert!(step(&mut store, &hub, &hub, &config(), &repo(), &settings()).is_err());
    assert_eq!(store.ledger().candidate(id).unwrap().state, State::Green);
    hub.heal(Call::Mark);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.verdicts, [(id, Verdict::Dropped)]);
    assert_eq!(hub.latest(&head(4)), Some(Mark::Waiting));
    assert!(stepped.landed.is_empty() && hub.landed(REMI).is_empty());

    // The watchdog dropping a green candidate does the same.
    let (again, _) = gating(&stepped);
    hub.ci(&commit_of(&store, again), &[("check / check", "success")]);
    hub.fail(unmarked(Mark::Landing));
    assert!(step(&mut store, &hub, &hub, &config(), &repo(), &settings()).is_err());
    let state = |store: &Store, id| store.ledger().candidate(id).unwrap().state.clone();
    assert_eq!(state(&store, again), State::Green);
    hub.fail(unmarked(Mark::Waiting));
    assert!(drop_candidate(&mut store, &hub, &hub, &repo(), again).is_err());
    assert_eq!(state(&store, again), State::Green);
    hub.heal(Call::Mark);
    drop_candidate(&mut store, &hub, &hub, &repo(), again).unwrap();
    assert_eq!(state(&store, again), State::Dropped);
    assert_eq!(hub.latest(&head(4)), Some(Mark::Waiting));

    // But not while a landing of it is open: its head says it's landing, so
    // its merge may be on trunk. Nothing in it leaves then either. A step
    // settles it.
    let (third, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, third), &[("check / check", "success")]);
    hub.fail(Call::Land);
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
    hub.heal(Call::Land);
    assert_eq!(step_once(&mut store, &hub).landed, [4]);

    // Leaving otherwise puts the head to pending first too.
    assert_eq!(
        try_join(&mut store, &hub, 3, &head(3)).unwrap(),
        Joined::Joined
    );
    hub.fail(unmarked(Mark::Waiting));
    assert!(leave(&mut store, &hub, &hub, &repo(), 3, Left::Unready).is_err());
    assert_eq!(joined(&store), [3]);
    hub.heal(Call::Mark);
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
    let hub = github(&[3, 5]);
    join_all(&mut store, &hub, &[3, 5]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    hub.fail(Call::Land);
    assert!(step(&mut store, &hub, &hub, &config(), &repo(), &settings()).is_err());
    hub.heal(Call::Land);
    assert_eq!(hub.latest(&head(3)), Some(Mark::Landing));
    // Nothing in it leaves meanwhile, the second no more than the first.
    assert_eq!(
        leave(&mut store, &hub, &hub, &repo(), 5, Left::Unready).unwrap(),
        Leaving::Landing(3)
    );
    hub.push(REMI, 5, &head(55));
    ingest::pull(&mut store, &hub, &config(), &repo(), 5).unwrap();
    assert!(matches!(
        store.ledger().candidate(id).unwrap().state,
        State::Broken { .. }
    ));
    // Until GitHub takes that, nothing new is built.
    hub.fail(unmarked(Mark::Waiting));
    assert!(step(&mut store, &hub, &hub, &config(), &repo(), &settings()).is_err());
    assert_eq!(store.ledger().train(&repo()).unwrap().candidates().len(), 1);
    hub.heal(Call::Mark);
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
    let hub = github(&[4]);
    r2(&hub, 4);
    assert_eq!(
        try_join(&mut store, &hub, 4, &head(4)).unwrap(),
        Joined::Joined
    );
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    hub.stale(Call::Pull.pr(4));
    assert_eq!(step_once(&mut store, &hub).landed, [4]);
    let stepped = step_once(&mut store, &hub);
    assert!(stepped.verdicts.is_empty() && stepped.refused.is_empty());
    assert!(matches!(stepped.now, Now::Landing { candidate, .. } if candidate == id));
    hub.fresh(Call::Pull);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.finished, [id]);
    assert!(store.ledger().candidate(id).unwrap().same_tree());
}

#[test]
fn a_merge_github_shows_before_linking_it_is_waited_for_and_lands_through_its_candidate() {
    // GitHub shows the PR merged a moment before it links the squash to
    // the PR, as it did for remi's first two landings through its train.
    // That's no outside merge: the train neither lets it go nor merges it
    // again, and it lands through its candidate once GitHub links it.
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[3, 5]);
    join_all(&mut store, &hub, &[3, 5]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    hub.stale(Call::CommitPulls);
    for (landed, verdicts) in [(vec![3], vec![(id, Verdict::Green)]), (vec![], vec![])] {
        let stepped = step_once(&mut store, &hub);
        assert_eq!((stepped.landed, stepped.verdicts), (landed, verdicts));
        assert!(stepped.refused.is_empty());
        assert!(matches!(stepped.now, Now::Landing { candidate, .. } if candidate == id));
        assert_eq!(hub.latest(&head(3)), Some(Mark::Landing));
    }
    hub.fresh(Call::CommitPulls);
    let stepped = step_once(&mut store, &hub);
    assert_eq!((stepped.landed, stepped.finished), (vec![5], vec![id]));
    for pr in [3, 5] {
        let c = store.ledger().by_pr(&repo(), pr).unwrap();
        assert_eq!(c.landing.as_ref().unwrap().candidate, Some(id));
    }
    assert_eq!(
        said(&hub, &head(3)),
        Some((Mark::Waiting, format!("landed with candidate {id}")))
    );
    assert!(store.ledger().outside().is_empty());
    assert_eq!(hub.held(REMI, "main"), [3, 5].map(head));
}

#[test]
fn a_merge_github_shows_before_linking_it_when_its_candidate_stopped_lands_through_it() {
    // The same, with the second's new head stopping the candidate while
    // GitHub hasn't linked the first's merge yet.
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[3, 5]);
    join_all(&mut store, &hub, &[3, 5]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    hub.stale(Call::CommitPulls);
    assert_eq!(step_once(&mut store, &hub).landed, [3]);
    hub.push(REMI, 5, &head(55));
    ingest::pull(&mut store, &hub, &config(), &repo(), 5).unwrap();
    let stepped = step_once(&mut store, &hub);
    assert!(
        matches!(stepped.now, Now::Landing { candidate, ref landings }
        if candidate == id && landings.iter().map(|l| l.pr).collect::<Vec<_>>() == [3])
    );
    assert_eq!(hub.latest(&head(3)), Some(Mark::Landing));
    hub.fresh(Call::CommitPulls);
    step_once(&mut store, &hub);
    let three = store.ledger().by_pr(&repo(), 3).unwrap();
    assert_eq!(three.landing.as_ref().unwrap().candidate, Some(id));
    assert_eq!(
        said(&hub, &head(3)),
        Some((Mark::Waiting, format!("landed with candidate {id}")))
    );
    assert!(store.ledger().outside().is_empty());
    assert_eq!(hub.held(REMI, "main"), [head(3)]);
}

#[test]
fn a_merge_whose_answer_was_lost_is_never_made_again() {
    // GitHub's answer to the merge lost, and the merge not shown yet when
    // the next step comes: it waits for GitHub to show it.
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[3, 5]);
    join_all(&mut store, &hub, &[3, 5]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    hub.lose(Call::Land);
    hub.stale(Call::Pull.pr(3));
    assert!(step(&mut store, &hub, &hub, &config(), &repo(), &settings()).is_err());
    hub.heal(Call::Land);
    let stepped = step_once(&mut store, &hub);
    assert!(stepped.landed.is_empty() && stepped.refused.is_empty());
    assert!(matches!(stepped.now, Now::Landing { candidate, .. } if candidate == id));
    hub.fresh(Call::Pull);
    let stepped = step_once(&mut store, &hub);
    assert_eq!((stepped.landed, stepped.finished), (vec![5], vec![id]));
    assert_eq!(hub.held(REMI, "main"), [3, 5].map(head));
}

#[test]
fn a_landed_heads_success_is_retired_before_the_next_head_says_its_landing() {
    // Once a head has landed, its train status says so, as pending: another
    // PR at that head (opened again after a revert, say) can't merge on it.
    // GitHub not taking that stops the next landing until it does.
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github(&[3, 5]);
    join_all(&mut store, &hub, &[3, 5]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    hub.fail(unmarked(Mark::Waiting));
    assert!(step(&mut store, &hub, &hub, &config(), &repo(), &settings()).is_err());
    assert_eq!(hub.held(REMI, "main"), [head(3)]);
    assert_eq!(hub.latest(&head(5)), Some(Mark::Waiting));
    hub.heal(Call::Mark);
    let stepped = step_once(&mut store, &hub);
    assert_eq!((stepped.landed, stepped.finished), (vec![5], vec![id]));
    let landed = Some((Mark::Waiting, format!("landed with candidate {id}")));
    assert_eq!(said(&hub, &head(3)), landed);
    assert_eq!(said(&hub, &head(5)), landed);
    // The first was retired before the second said it was landing.
    let at = |sha: &str, mark: Mark| {
        hub.marks()
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
    let hub = github(&[3, 5, 7]);
    join_all(&mut store, &hub, &[3, 5, 7]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    hub.stale(Call::Pull.pr(3));
    assert_eq!(step_once(&mut store, &hub).landed, [3]);
    hub.push(REMI, 5, &head(55));
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
    hub.fresh(Call::Pull);
    let stepped = step_once(&mut store, &hub);
    assert!(stepped.landed.is_empty() && stepped.now == Now::Idle);
    let three = store.ledger().by_pr(&repo(), 3).unwrap();
    assert_eq!(three.landing.as_ref().unwrap().candidate, Some(id));
    assert_eq!(
        said(&hub, &head(3)),
        Some((Mark::Waiting, format!("landed with candidate {id}")))
    );
    assert_eq!(hub.held(REMI, "main"), [head(3)]);
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
        let hub = github(&[3, 5]);
        if settled {
            let theirs = hub.others_statuses(&head(3));
            hub.put_statuses(&head(33), theirs);
        }
        join_all(&mut store, &hub, &[3, 5]);
        let (id, _) = gating(&step_once(&mut store, &hub));
        hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
        hub.fail(Call::Land);
        assert!(step(&mut store, &hub, &hub, &config(), &repo(), &settings()).is_err());
        hub.heal(Call::Land);
        hub.push(REMI, 5, &head(55));
        ingest::pull(&mut store, &hub, &config(), &repo(), 5).unwrap();
        hub.push(REMI, 3, &head(33));
        assert_eq!(
            hub.land("FieldmouseWorks/remi", 3, &head(33), &message())
                .unwrap(),
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
        assert_eq!(hub.held(REMI, "main"), [head(33)]);
    }
}

/// The steward's word on a head, newest first, then the success it said
/// when the PR joined.
fn then_said(hub: &Hub, pr: u64, state: &str, by: &str) {
    hub.put_statuses(&head(pr),
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
    let hub = github(&[3, 4, 6]);
    join_all(&mut store, &hub, &[3, 4, 6]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    // Held once its candidate was green: GitHub would merge it all the same.
    then_said(&hub, 4, "pending", STEWARD);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(
        (&stepped.landed[..], &stepped.unready[..]),
        (&[3][..], &[4][..])
    );
    assert_eq!(hub.held(REMI, "main"), [head(3)]);
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
    let hub = github(&[3, 4]);
    join_all(&mut store, &hub, &[3, 4]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    // Anyone who can push can post the steward's context.
    then_said(&hub, 4, "pending", "someone");
    let stepped = step_once(&mut store, &hub);
    assert_eq!((stepped.landed, stepped.unready), (vec![3, 4], vec![]));
}

#[test]
fn a_landing_says_the_prs_title_and_each_distinct_trailer_of_its_commits_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let author = "A Bot <bot@example.com>";
    // #5 took three rounds, each commit with the same co-author, spelled
    // two ways, and a session line of its own or the first one again.
    let first = own_commit(1);
    let second = own_commit(2);
    let hub = Hub::new(REMI, TRUNK);
    hub.change(&first, TRUNK, "five, first");
    hub.change(&second, &first, "five, second");
    hub.change(&head(5), &second, &head(5));
    for (sha, message) in [
        (
            first.clone(),
            format!(
                "remi: land it\n\nWhy.\n\nCo-Authored-By: {author}\nClaude-Session: https://s/1"
            ),
        ),
        (
            second.clone(),
            format!("remi: answer\n\nCo-authored-by: {author}\nClaude-Session: https://s/2"),
        ),
        (
            head(5),
            format!("remi: again\n\nCo-Authored-By: {author}\nClaude-Session: https://s/1"),
        ),
    ] {
        hub.commit_details(&sha, Some(&message), None, None);
    }
    hub.pr(REMI, 5, &head(5));
    hub.edit_pull(REMI, 5, |p| {
        p["title"] = json!("remi: land it in three rounds")
    });
    // #3 is one commit, whose subject isn't its PR's title.
    hub.pr(REMI, 3, &head(3));
    hub.commit_details(
        &head(3),
        Some(&format!(
            "remi: an older subject\n\nCo-authored-by: {author}"
        )),
        None,
        None,
    );
    hub.edit_pull(REMI, 3, |p| {
        p["title"] = json!("remi: the title the house checked")
    });
    join_all(&mut store, &hub, &[5, 3]);
    let (id, _) = gating(&step_once(&mut store, &hub));
    hub.ci(&commit_of(&store, id), &[("check / check", "success")]);
    let stepped = step_once(&mut store, &hub);
    assert_eq!(stepped.landed, [5, 3]);
    let sent: Vec<(String, String)> = hub
        .asked(Call::Land)
        .into_iter()
        .map(|a| (a[3].clone(), a[4].clone()))
        .collect();
    let five = (
        "remi: land it in three rounds (#5)".to_owned(),
        format!(
            "Co-Authored-By: {author}\nClaude-Session: https://s/1\nClaude-Session: https://s/2"
        ),
    );
    let three = (
        "remi: the title the house checked (#3)".to_owned(),
        format!("Co-authored-by: {author}"),
    );
    assert_eq!(sent, [five.clone(), three.clone()]);
    // What landed on trunk says just that.
    for (pr, (title, body)) in [(5, five), (3, three)] {
        let merged = hub.merged(REMI, pr).unwrap();
        assert_eq!(
            hub.commit(REMI, &merged).unwrap().commit.unwrap().message,
            Some(format!("{title}\n\n{body}"))
        );
    }
}

/// A commit of a PR's own, before its head.
fn own_commit(n: u64) -> String {
    format!("c{n:0>39}")
}
