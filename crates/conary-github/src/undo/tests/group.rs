//! Undo by session and by time: one PR per repo, reverting each change
//! newest first.

use super::*;

const SESSION: &str = "https://claude.ai/code/session_01Bad";

/// Changes landed one after another on trunk, in `repo`, each by the
/// session given for it, as PRs from `first` up: their ids, oldest first.
fn chain(
    store: &mut Store,
    hub: &Hub,
    repo: &Repo,
    first: u64,
    sessions: &[Option<&str>],
) -> Vec<ChangeId> {
    let mut ids = Vec::new();
    for (i, session) in sessions.iter().enumerate() {
        let pr = first + i as u64;
        let before = hub.branches.borrow()["main"].clone();
        let after = hub.sha();
        hub.trees
            .borrow_mut()
            .insert(after.clone(), format!("trunk with #{pr}"));
        hub.parents
            .borrow_mut()
            .insert(after.clone(), vec![before.clone()]);
        hub.branches
            .borrow_mut()
            .insert("main".into(), after.clone());
        let change = ChangeId::new().unwrap();
        store
            .record(Body::Observe {
                change,
                repo: repo.clone(),
                pr,
                author: Actor {
                    kind: ActorKind::Agent,
                    login: "TusanHomichi".into(),
                    session: session.map(String::from),
                },
            })
            .unwrap();
        store
            .record(Body::Intent {
                change,
                summary: format!("tools: change {pr}"),
                body: String::new(),
                task: None,
            })
            .unwrap();
        land(store, change, &before, &after);
        // Landings a millisecond apart are told apart by their times.
        std::thread::sleep(std::time::Duration::from_millis(2));
        ids.push(change);
    }
    ids
}

fn burrow() -> Repo {
    "FieldmouseWorks/burrow".parse().unwrap()
}

fn actor(store: &mut Store, hub: &Hub) -> Result<Undid, Stopped> {
    undo_actor(store, hub, hub, &config(), SESSION)
}

#[test]
fn a_session_s_changes_are_undone_by_one_pr_in_each_repo_newest_first() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new();
    let s = Some(SESSION);
    // Another session's change landed between the bad session's two here.
    let remi = chain(&mut store, &hub, &repo(), 10, &[s, Some("other"), s]);
    let burrow_ids = chain(&mut store, &hub, &burrow(), 20, &[s]);
    let got = actor(&mut store, &hub).unwrap();
    assert_eq!(got.undos.len(), 2);
    let in_repo = |r: &Repo| {
        got.undos
            .iter()
            .find(|g| &store.ledger().change(g.change).unwrap().repo == r)
            .unwrap()
    };
    let (r, b) = (in_repo(&repo()), in_repo(&burrow()));
    assert_eq!(r.reverts, [remi[2], remi[0]]);
    assert_eq!(b.reverts, [burrow_ids[0]]);
    assert!(r.left.is_empty());
    // Each change's own revert, the newest's the branch's base: the other
    // session's change isn't one of them.
    let made: Vec<String> = hub.made.borrow().iter().map(|m| m.1.clone()).collect();
    let after = |id| {
        store
            .ledger()
            .change(id)
            .unwrap()
            .landing
            .as_ref()
            .unwrap()
            .after
            .to_string()
    };
    assert!(made.contains(&after(remi[2])) && made.contains(&after(remi[0])));
    assert!(!made.contains(&after(remi[1])));
    let u = store.ledger().change(r.change).unwrap();
    assert_eq!(
        u.undo.as_ref().unwrap().asked,
        Asked::Actor {
            session: SESSION.into()
        }
    );
    assert_eq!(store.ledger().undo_of(remi[0]), Some(r.change));
    assert_eq!(store.ledger().undo_of(remi[1]), None);
    let pr = hub.pull_json(r.pr);
    assert_eq!(
        pr["title"],
        format!("undo: revert 2 changes from session {SESSION}")
    );
    assert_eq!(pr["head"]["ref"], undo_branch(remi[2], 1));
    // Run again, it finds both.
    let again = actor(&mut store, &hub).unwrap();
    assert!(again.undos.iter().all(|g| g.how == How::Found), "{again:?}");
    assert_eq!(hub.pulls.borrow().len(), 2);
}

#[test]
fn a_revert_that_conflicts_with_the_others_is_left_for_an_undo_of_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new();
    let s = Some(SESSION);
    let ids = chain(&mut store, &hub, &repo(), 10, &[s, s, s]);
    // The oldest one's revert, the tree before it, doesn't merge.
    let before = store
        .ledger()
        .change(ids[0])
        .unwrap()
        .landing
        .clone()
        .unwrap()
        .before;
    let tree = hub.trees.borrow()[before.as_str()].clone();
    hub.conflicting.borrow_mut().push(tree);
    let got = actor(&mut store, &hub).unwrap();
    assert_eq!(got.undos.len(), 1);
    assert_eq!(
        (got.undos[0].reverts.clone(), got.undos[0].left.clone()),
        (vec![ids[2], ids[1]], vec![ids[0]])
    );
    assert_eq!(store.ledger().undo_of(ids[0]), None);
    let body = hub.pull_json(got.undos[0].pr)["body"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(body.contains("Left out"), "{body}");
    assert!(body.contains(&ids[0].to_string()));
    // Run again: the first is found, and the one left out gets its own,
    // its revert the branch's base.
    hub.conflicting.borrow_mut().clear();
    let again = actor(&mut store, &hub).unwrap();
    assert_eq!(again.undos.len(), 2);
    assert_eq!(again.undos[0].how, How::Found);
    assert_eq!(again.undos[1].reverts, [ids[0]]);
    assert_eq!(store.ledger().undo_of(ids[0]), Some(again.undos[1].change));
}

#[test]
fn since_undoes_what_landed_on_the_repo_after_the_time() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new();
    let ids = chain(&mut store, &hub, &repo(), 10, &[None, Some("a"), Some("b")]);
    chain(&mut store, &hub, &burrow(), 20, &[None]);
    let since = store
        .ledger()
        .change(ids[0])
        .unwrap()
        .landing
        .as_ref()
        .unwrap()
        .at;
    let got = undo_since(&mut store, &hub, &hub, &config(), &repo(), since).unwrap();
    assert_eq!(got.undos.len(), 1);
    assert_eq!(got.undos[0].reverts, [ids[2], ids[1]]);
    let u = store.ledger().change(got.undos[0].change).unwrap();
    assert_eq!(u.undo.as_ref().unwrap().asked, Asked::Since { since });
    let title = hub.pull_json(got.undos[0].pr)["title"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        title.starts_with("undo: revert 2 changes since 20"),
        "{title}"
    );
    // Nothing after the newest.
    let last = store
        .ledger()
        .change(ids[2])
        .unwrap()
        .landing
        .as_ref()
        .unwrap()
        .at;
    let err = undo_since(&mut store, &hub, &hub, &config(), &repo(), last).unwrap_err();
    assert!(matches!(err.error, UndoError::NoneLanded(_)), "{err}");
    assert!(err.refused());
}

#[test]
fn a_session_s_run_that_stops_in_one_repo_keeps_the_undos_it_opened_before() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new();
    let s = Some(SESSION);
    chain(&mut store, &hub, &burrow(), 20, &[s]);
    chain(&mut store, &hub, &repo(), 10, &[s]);
    // burrow's turn comes first; then GitHub errs for remi.
    hub.down.borrow_mut().push(repo().to_string());
    let err = actor(&mut store, &hub).unwrap_err();
    assert!(matches!(err.error, UndoError::Api(_)), "{err}");
    assert!(!err.refused());
    assert_eq!(err.done.undos.len(), 1);
    let opened = &err.done.undos[0];
    assert_eq!(hub.pulls.borrow().len(), 1);
    assert_eq!(hub.pull_json(opened.pr)["number"], opened.pr);
    assert_eq!(store.ledger().change(opened.change).unwrap().repo, burrow());
    // Asked again once GitHub's back, it finds that one and opens remi's.
    hub.down.borrow_mut().clear();
    let got = actor(&mut store, &hub).unwrap();
    assert_eq!(got.undos.len(), 2);
    assert!(
        got.undos
            .iter()
            .any(|u| u.pr == opened.pr && u.how == How::Found)
    );
    assert_eq!(hub.pulls.borrow().len(), 2);
}

#[test]
fn a_time_s_undo_in_a_repo_the_ledger_keeps_nothing_of_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new();
    chain(&mut store, &hub, &repo(), 10, &[None]);
    let stranger: Repo = "someone/else".parse().unwrap();
    // Anything asked of GitHub about it errs.
    hub.down.borrow_mut().push(stranger.to_string());
    hub.pulls.borrow_mut().clear();
    let err = undo_since(&mut store, &hub, &hub, &config(), &stranger, 0).unwrap_err();
    assert!(
        matches!(&err.error, UndoError::Unkept(r) if r == &stranger),
        "{err}"
    );
    assert!(err.refused());
    assert_eq!(
        err.to_string(),
        "the ledger keeps no change of someone/else"
    );
}

#[test]
fn what_the_ledger_refuses_is_told_apart_from_what_stopped_a_run() {
    let id = ChangeId::new().unwrap();
    for e in [
        UndoError::Unknown(id),
        UndoError::NotLanded(id),
        UndoError::Nothing(id),
        UndoError::NoTrunk("main".into()),
        UndoError::NoneLanded(Asked::Change),
        UndoError::Unkept(repo()),
        UndoError::Taken {
            branch: "conary/undo/x/1".into(),
            pr: 3,
        },
    ] {
        assert!(e.refused(), "{e}");
        assert!(Stopped::from(e).refused());
    }
    assert!(!UndoError::Api(ApiError::new("Server Error")).refused());
    // A refusal after an undo was opened is a run that stopped partway.
    let mut stopped = Stopped::from(UndoError::NoTrunk("main".into()));
    stopped.done.undos.push(Undone {
        change: id,
        pr: 7,
        reverts: vec![id],
        left: Vec::new(),
        how: How::Opened { conflicts: false },
        recorded: 1,
    });
    assert!(!stopped.refused());
}

#[test]
fn a_stopped_run_s_pr_for_a_session_is_its_undo_read_from_its_marker() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new();
    let s = Some(SESSION);
    let ids = chain(&mut store, &hub, &repo(), 10, &[s, s]);
    // A run opened the PR and stopped before recording it.
    let marker = Marker {
        reverts: vec![ids[1], ids[0]],
        asked: Asked::Actor {
            session: SESSION.into(),
        },
    };
    let revert = hub.make_commit("", "x", AFTER, "m").unwrap();
    hub.create_branch("", &undo_branch(ids[1], 1), &revert)
        .unwrap();
    let pr = hub
        .open_pull("", &undo_branch(ids[1], 1), "main", "t", &marker.line())
        .unwrap();
    let got = actor(&mut store, &hub).unwrap();
    assert_eq!(got.undos.len(), 1);
    assert_eq!((got.undos[0].pr, got.undos[0].how), (pr, How::Found));
    assert_eq!(got.undos[0].reverts, [ids[1], ids[0]]);
    assert_eq!(store.ledger().undo_of(ids[0]), Some(got.undos[0].change));
}

/// The tree `id`'s revert makes: trunk's before it landed.
fn reverted(store: &Store, hub: &Hub, id: ChangeId) -> String {
    let landing = store.ledger().change(id).unwrap().landing.clone().unwrap();
    hub.trees.borrow()[landing.before.as_str()].clone()
}

#[test]
fn an_actor_s_undo_isn_t_the_actor_s_change_once_it_lands() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new();
    let s = Some(SESSION);
    chain(&mut store, &hub, &repo(), 10, &[s, s]);
    let got = actor(&mut store, &hub).unwrap();
    let undo = store.ledger().change(got.undos[0].change).unwrap();
    // Its marker names the session it reverts; its author is who opened it.
    assert_eq!(undo.author.session, None);
    hub.land(got.undos[0].pr);
    // Landed, the session has nothing left to undo: the undo isn't its.
    let err = actor(&mut store, &hub).unwrap_err();
    assert!(
        matches!(err.error, UndoError::NoneLanded(Asked::Actor { .. })),
        "{err}"
    );
    assert_eq!(hub.pulls.borrow().len(), 1);
}

#[test]
fn a_stopped_run_s_pr_is_found_through_any_change_it_reverts() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new();
    let s = Some(SESSION);
    let ids = chain(&mut store, &hub, &repo(), 10, &[s, s]);
    // A run opened the PR on the newest's branch and stopped before
    // recording it; the older one is asked for alone.
    let marker = Marker {
        reverts: vec![ids[1], ids[0]],
        asked: Asked::Actor {
            session: SESSION.into(),
        },
    };
    let revert = hub.make_commit("", "x", AFTER, "m").unwrap();
    hub.create_branch("", &undo_branch(ids[1], 1), &revert)
        .unwrap();
    let pr = hub
        .open_pull("", &undo_branch(ids[1], 1), "main", "t", &marker.line())
        .unwrap();
    let got = undo(&mut store, &hub, &hub, &config(), ids[0]).unwrap();
    assert_eq!((got.pr, got.how), (pr, How::Found));
    assert_eq!(got.reverts, [ids[1], ids[0]]);
    assert_eq!(store.ledger().undo_of(ids[1]), Some(got.change));
    assert_eq!(hub.pulls.borrow().len(), 1);
}

#[test]
fn a_stopped_run_s_pr_merged_and_its_branch_gone_is_found_all_the_same() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new();
    let s = Some(SESSION);
    let ids = chain(&mut store, &hub, &repo(), 10, &[s, s]);
    let marker = Marker {
        reverts: vec![ids[1], ids[0]],
        asked: Asked::Actor {
            session: SESSION.into(),
        },
    };
    let revert = hub.make_commit("", "x", AFTER, "m").unwrap();
    hub.create_branch("", &undo_branch(ids[1], 1), &revert)
        .unwrap();
    let pr = hub
        .open_pull("", &undo_branch(ids[1], 1), "main", "t", &marker.line())
        .unwrap();
    // It landed and its branch went, and trunk has what the older one
    // brought, as far as a revert of it can tell: its undo landed all the
    // same.
    hub.land(pr);
    assert!(hub.branches("", "conary/").unwrap().is_empty());
    let err = undo(&mut store, &hub, &hub, &config(), ids[0]).unwrap_err();
    let by = store.ledger().by_pr(&repo(), pr).unwrap().id;
    assert!(
        matches!(err, UndoError::Reverted { change, by: b } if change == ids[0] && b == by),
        "{err}"
    );
    assert_eq!(hub.pulls.borrow().len(), 1);
}

#[test]
fn a_pr_from_the_branch_that_isn_t_an_undo_keeps_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new();
    let ids = chain(&mut store, &hub, &repo(), 10, &[None]);
    let branch = undo_branch(ids[0], 1);
    let made = hub.make_commit("", "x", AFTER, "m").unwrap();
    hub.create_branch("", &branch, &made).unwrap();
    let pr = hub
        .open_pull("", &branch, "main", "t", "its marker taken out")
        .unwrap();
    let err = undo(&mut store, &hub, &hub, &config(), ids[0]).unwrap_err();
    assert!(
        matches!(&err, UndoError::Taken { branch: b, pr: p } if b == &branch && *p == pr),
        "{err}"
    );
    assert_eq!(hub.branch("", &branch).unwrap(), Some(made));
}

#[test]
fn what_trunk_already_lacks_is_gone_and_holds_up_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new();
    let s = Some(SESSION);
    // Others took out burrow's, and on remi the newest's and the oldest's.
    let b = chain(&mut store, &hub, &burrow(), 20, &[s]);
    let r = chain(&mut store, &hub, &repo(), 10, &[s, s, s, Some("other")]);
    for id in [b[0], r[2], r[0]] {
        let tree = reverted(&store, &hub, id);
        hub.gone.borrow_mut().push(tree);
    }
    let got = actor(&mut store, &hub).unwrap();
    assert_eq!(got.gone, [b[0], r[2], r[0]]);
    assert_eq!(got.undos.len(), 1);
    // The middle one's revert is the branch's base, and it alone is undone.
    assert_eq!(got.undos[0].reverts, [r[1]]);
    assert_eq!(
        hub.pull_json(got.undos[0].pr)["head"]["ref"],
        undo_branch(r[1], 1)
    );
    for id in [r[2], r[0]] {
        assert_eq!(store.ledger().undo_of(id), None);
    }
    let body = hub.pull_json(got.undos[0].pr)["body"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(body.contains("already lacks") && body.contains(&r[0].to_string()));
    // Nothing that's gone is left a branch.
    assert_eq!(hub.branches("", "conary/").unwrap(), [undo_branch(r[1], 1)]);
}

#[test]
fn reverts_that_together_change_nothing_leave_what_conflicted_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new();
    let s = Some(SESSION);
    let ids = chain(&mut store, &hub, &repo(), 10, &[s, s, s, Some("other")]);
    // The newer two's reverts make trunk as it is, and the oldest's
    // conflicts with them.
    let trunk = hub.branches.borrow()["main"].clone();
    let merging = hub.merging.borrow().clone().unwrap();
    let net = format!("{merging}, then {}", reverted(&store, &hub, ids[1]));
    hub.trees.borrow_mut().insert(trunk, net);
    let tree = reverted(&store, &hub, ids[0]);
    hub.conflicting.borrow_mut().push(tree);
    let got = actor(&mut store, &hub).unwrap();
    assert_eq!(got.gone, [ids[2], ids[1]]);
    assert_eq!(got.undos.len(), 1);
    assert_eq!(got.undos[0].reverts, [ids[0]]);
    assert_eq!(store.ledger().undo_of(ids[2]), None);
}

#[test]
fn reverts_that_change_nothing_together_are_gone_though_trunk_conflicted_with_the_first() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new();
    let s = Some(SESSION);
    let ids = chain(&mut store, &hub, &repo(), 10, &[s, s, Some("other")]);
    // Trunk won't merge into the newer one's revert, and with the older
    // one's merged in, the branch is trunk as it is.
    let net = format!(
        "{}, then {}",
        reverted(&store, &hub, ids[1]),
        reverted(&store, &hub, ids[0])
    );
    let trunk = hub.branches.borrow()["main"].clone();
    hub.trees.borrow_mut().insert(trunk, net.clone());
    hub.conflicting.borrow_mut().push(net);
    let got = actor(&mut store, &hub).unwrap();
    assert!(got.undos.is_empty(), "{got:?}");
    assert_eq!(got.gone, [ids[1], ids[0]]);
    assert!(hub.pulls.borrow().is_empty());
    assert!(hub.branches("", "conary/").unwrap().is_empty());
}

#[test]
fn a_session_that_landed_nothing_has_nothing_to_undo() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = Hub::new();
    chain(&mut store, &hub, &repo(), 10, &[Some("other")]);
    let err = actor(&mut store, &hub).unwrap_err();
    assert!(
        matches!(err.error, UndoError::NoneLanded(Asked::Actor { .. })),
        "{err}"
    );
}

#[test]
fn times_read_as_utc() {
    assert_eq!(time(0), "1970-01-01T00:00:00Z");
    assert_eq!(time(1_793_750_400_000), "2026-11-04T00:00:00Z");
    assert_eq!(time(951_782_400_000 + 3_723_000), "2000-02-29T01:02:03Z");
}
