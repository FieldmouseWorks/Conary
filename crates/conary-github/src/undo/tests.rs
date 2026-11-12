use ed25519_dalek::SigningKey;

use super::*;
use crate::sim::{Call, Hub, REMI};
use conary_core::ledger::Status as Kept;
use conary_core::op::{Actor, ActorKind, Authority};

/// Trunk before the change landed, the change's landing, and trunk now.
const BEFORE: &str = "1111111111111111111111111111111111111111";
const AFTER: &str = "2222222222222222222222222222222222222222";
const TRUNK: &str = "3333333333333333333333333333333333333333";
use crate::sim::STEWARD;

fn repo() -> Repo {
    "FieldmouseWorks/remi".parse().unwrap()
}

fn config() -> Config {
    Config {
        reviewers: vec![STEWARD.into()],
        own_contexts: vec!["review-settled".into()],
    }
}

/// PR 5's change, and what landed on trunk after it.
const CHANGE: &str = "#5";
const LATER: &str = "later";

/// GitHub as undo finds it: in remi, trunk before PR 5's change landed
/// (BEFORE, holding nothing), its landing (AFTER) and trunk now (TRUNK,
/// with something LATER on top); and burrow, its trunk at BEFORE too.
fn github() -> Hub {
    let hub = Hub::new(REMI, BEFORE);
    hub.change(AFTER, BEFORE, CHANGE);
    hub.change(TRUNK, AFTER, LATER);
    hub.set_branch(REMI, "main", TRUNK);
    hub.add_repo(BURROW, "main", BEFORE);
    hub
}

const BURROW: &str = "FieldmouseWorks/burrow";

/// Every PR opened, in either repo.
fn pulls(hub: &Hub) -> usize {
    [REMI, BURROW]
        .map(|r| hub.pull_numbers(r).len())
        .iter()
        .sum()
}

/// A revert of PR 5's change, as a run makes it: trunk's tree before it
/// landed, on its landing.
fn a_revert(hub: &Hub) -> String {
    hub.make_commit(REMI, &hub.tree_at(BEFORE), AFTER, "m")
        .unwrap()
}

fn store(dir: &tempfile::TempDir) -> Store {
    let lock = conary_core::log::Lock::take(dir.path()).unwrap();
    Store::open(lock, SigningKey::from_bytes(&[5; 32]), None).unwrap()
}

/// A change in the ledger, PR 5, landed as AFTER on BEFORE, titled `title`.
fn landed(store: &mut Store, title: Option<&str>) -> ChangeId {
    let change = ChangeId::new().unwrap();
    store
        .record(Body::Observe {
            change,
            repo: repo(),
            pr: 5,
            author: Actor {
                kind: ActorKind::Agent,
                login: "TusanHomichi".into(),
                session: None,
            },
        })
        .unwrap();
    if let Some(title) = title {
        store
            .record(Body::Intent {
                change,
                summary: title.into(),
                body: String::new(),
                task: None,
            })
            .unwrap();
    }
    land(store, change, BEFORE, AFTER);
    change
}

fn land(store: &mut Store, change: ChangeId, before: &str, after: &str) {
    if store.ledger().change(change).unwrap().current.is_none() {
        store
            .record(Body::Revision {
                change,
                base: BEFORE.parse().unwrap(),
                head: "4444444444444444444444444444444444444444".parse().unwrap(),
            })
            .unwrap();
    }
    let revision = store.ledger().change(change).unwrap().current.unwrap();
    store
        .record(Body::Land {
            change,
            revision,
            before: before.parse().unwrap(),
            after: after.parse().unwrap(),
            by: Authority::Status {
                login: STEWARD.into(),
            },
        })
        .unwrap();
}

fn undo_of(store: &mut Store, hub: &Hub, x: ChangeId) -> Result<Undone, Stopped> {
    undo(store, hub, hub, &config(), x)
}

#[test]
fn an_undo_reverts_the_landing_alone_brings_trunk_in_and_is_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github();
    let x = landed(
        &mut store,
        Some("autoland: merge only the head that was checked"),
    );
    let got = undo_of(&mut store, &hub, x).unwrap();
    assert_eq!(got.how, How::Opened { conflicts: false });
    assert_eq!((got.pr, got.reverts.clone()), (100, vec![x]));
    // The revert: trunk's tree from before it landed, on its landing.
    let title = "autoland: undo #5, merge only the head that was checked";
    assert_eq!(
        hub.made(),
        [(
            hub.tree_at(BEFORE),
            AFTER.to_owned(),
            format!("{title}\n\nThis reverts {AFTER}, #5's landing.")
        )]
    );
    // Its branch, with trunk merged in, is the PR's head.
    let branch = undo_branch(x, 1);
    assert_eq!(branch, format!("conary/undo/{x}/1"));
    let pull = hub.pull_json(REMI, 100);
    assert_eq!(pull["head"]["ref"], branch);
    assert_eq!(pull["base"]["ref"], "main");
    assert_eq!(pull["title"], title);
    let body = pull["body"].as_str().unwrap();
    assert!(body.starts_with(&format!(
        "Undoes #5, which landed as {AFTER} (Conary change `{x}`)"
    )));
    assert!(!body.contains("conflicts"));
    let head = hub.branch_at(REMI, &branch).unwrap();
    // Trunk without the change.
    assert_eq!(hub.holding(&head), [LATER]);
    // The ledger holds it as the change's undo, asked for by the change.
    let u = store.ledger().change(got.change).unwrap();
    assert_eq!(u.pr, 100);
    let recorded = u.undo.as_ref().unwrap();
    assert_eq!(
        (recorded.reverts.clone(), recorded.asked.clone()),
        (vec![x], Asked::Change)
    );
    assert_eq!(store.ledger().undo_of(x), Some(got.change));
    assert!(got.recorded > 0);
    // Run again, it finds it: nothing new made, opened or recorded.
    let again = undo_of(&mut store, &hub, x).unwrap();
    assert_eq!(
        (again.change, again.pr, again.how, again.recorded),
        (got.change, 100, How::Found, 0)
    );
    assert_eq!((hub.made().len(), pulls(&hub)), (1, 1));
}

#[test]
fn a_revert_that_conflicts_with_trunk_opens_as_it_is_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github();
    hub.clash(CHANGE, LATER);
    let x = landed(&mut store, Some("tools: a plain sentence"));
    let got = undo_of(&mut store, &hub, x).unwrap();
    assert_eq!(got.how, How::Opened { conflicts: true });
    let pull = hub.pull_json(REMI, got.pr);
    // The head is the revert alone, on the landing.
    let revert = hub.branch_at(REMI, &undo_branch(x, 1)).unwrap();
    assert_eq!(pull["head"]["sha"], revert);
    assert_eq!(hub.parent(&revert), AFTER);
    let body = pull["body"].as_str().unwrap();
    assert!(body.contains("this conflicts with main: merge main in"));
}

#[test]
fn a_change_whose_landing_is_trunk_has_its_revert_on_trunk_already() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github();
    hub.set_branch(REMI, "main", AFTER);
    let x = landed(&mut store, Some("tools: a plain sentence"));
    let got = undo_of(&mut store, &hub, x).unwrap();
    assert_eq!(got.how, How::Opened { conflicts: false });
    let head = hub.pull_json(REMI, got.pr)["head"]["sha"].clone();
    assert_eq!(hub.parent(head.as_str().unwrap()), AFTER);
}

#[test]
fn a_revert_that_changes_nothing_opens_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github();
    // Trunk took the change out some other way.
    let out = hub.take_out(TRUNK, CHANGE);
    hub.set_branch(REMI, "main", &out);
    let x = landed(&mut store, Some("tools: a plain sentence"));
    let err = *undo_of(&mut store, &hub, x).unwrap_err().error;
    assert!(matches!(err, UndoError::Nothing(c) if c == x), "{err}");
    assert_eq!(pulls(&hub), 0);
    assert!(hub.branch(REMI, &undo_branch(x, 1)).unwrap().is_none());
    assert_eq!(store.ledger().undo_of(x), None);
}

#[test]
fn a_run_that_stopped_partway_is_taken_up_where_it_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github();
    let x = landed(&mut store, Some("tools: a plain sentence"));
    // Its branch made, and no PR: it's made again, since what a stopped
    // run put on it needn't be what's asked for now.
    let earlier = a_revert(&hub);
    hub.create_branch(REMI, &undo_branch(x, 1), &earlier)
        .unwrap();
    let got = undo_of(&mut store, &hub, x).unwrap();
    assert_eq!(got.how, How::Opened { conflicts: false });
    let head = hub.pull_json(REMI, got.pr)["head"]["sha"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(!hub.reaches(&head, &earlier));
    assert_eq!(hub.made().len(), 2);

    // Its PR opened, and not recorded: it's recorded.
    let dir = tempfile::tempdir().unwrap();
    let mut fresh = self::store(&dir);
    let y = landed(&mut fresh, Some("tools: a plain sentence"));
    let pr = stopped(&hub, y);
    let got = undo_of(&mut fresh, &hub, y).unwrap();
    assert_eq!((got.pr, got.how), (pr, How::Found));
    assert!(got.recorded > 0);
    assert_eq!(fresh.ledger().undo_of(y), Some(got.change));
}

#[test]
fn an_undo_closed_leaves_the_next_a_branch_of_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github();
    let x = landed(&mut store, Some("tools: a plain sentence"));
    let first = undo_of(&mut store, &hub, x).unwrap();
    // Closed on GitHub, and asked again before anything reads it: it's read
    // first, so no closed PR is taken for the open undo.
    hub.close(REMI, first.pr);
    let second = undo_of(&mut store, &hub, x).unwrap();
    assert_eq!(
        store.ledger().change(first.change).unwrap().status(),
        Kept::Closed
    );
    assert_ne!(second.change, first.change);
    assert_eq!(second.how, How::Opened { conflicts: false });
    assert_eq!(
        hub.pull_json(REMI, second.pr)["head"]["ref"],
        undo_branch(x, 2)
    );
}

#[test]
fn an_undo_landed_on_github_is_read_before_another_is_made() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github();
    let x = landed(&mut store, Some("tools: a plain sentence"));
    let first = undo_of(&mut store, &hub, x).unwrap();
    hub.steward_lands(REMI, first.pr);
    assert!(matches!(
        undo_of(&mut store, &hub, x).map_err(|s| *s.error),
        Err(UndoError::Reverted { change, by }) if change == x && by == first.change
    ));
    assert_eq!(store.ledger().change(x).unwrap().status(), Kept::Reverted);
    assert_eq!(pulls(&hub), 1);
}

/// A run that opened its PR and stopped before recording it: the PR, on
/// `x`'s first undo branch, with its revert.
fn stopped(hub: &Hub, x: ChangeId) -> u64 {
    stopped_at(hub, x, 1)
}

/// The same, on the branch of `x`'s `attempt`th undo.
fn stopped_at(hub: &Hub, x: ChangeId, attempt: usize) -> u64 {
    let revert = a_revert(hub);
    hub.create_branch(REMI, &undo_branch(x, attempt), &revert)
        .unwrap();
    let marker = Marker {
        reverts: vec![x],
        asked: Asked::Change,
    };
    hub.open_pull(REMI, &undo_branch(x, attempt), "main", "t", &marker.line())
        .unwrap()
}

#[test]
fn a_stopped_run_s_pr_is_its_undo_whatever_became_of_it() {
    // Landed since, its branch gone: it's what reverted the change.
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github();
    let x = landed(&mut store, Some("tools: a plain sentence"));
    let pr = stopped(&hub, x);
    hub.steward_lands(REMI, pr);
    let err = *undo_of(&mut store, &hub, x).unwrap_err().error;
    let u = store.ledger().by_pr(&repo(), pr).unwrap().id;
    assert!(
        matches!(err, UndoError::Reverted { change, by } if change == x && by == u),
        "{err}"
    );
    assert_eq!(store.ledger().undo_of(x), Some(u));
    assert_eq!(pulls(&hub), 1);

    // Closed since: the next attempt takes a branch of its own.
    let dir = tempfile::tempdir().unwrap();
    let mut store = self::store(&dir);
    let hub = github();
    let y = landed(&mut store, Some("tools: a plain sentence"));
    let pr = stopped(&hub, y);
    hub.close(REMI, pr);
    let got = undo_of(&mut store, &hub, y).unwrap();
    let closed = store.ledger().by_pr(&repo(), pr).unwrap();
    assert_eq!(closed.status(), Kept::Closed);
    assert_eq!(
        store.ledger().change(y).unwrap().undone_by,
        [closed.id, got.change]
    );
    assert_eq!(
        hub.pull_json(REMI, got.pr)["head"]["ref"],
        undo_branch(y, 2)
    );

    // Landed, and undone in turn: undoing that is what reverts it again.
    let dir = tempfile::tempdir().unwrap();
    let mut store = self::store(&dir);
    let hub = github();
    let z = landed(&mut store, Some("tools: a plain sentence"));
    let pr = stopped(&hub, z);
    hub.steward_lands(REMI, pr);
    let u = ingest::pull(&mut store, &hub, &config(), &repo(), pr)
        .unwrap()
        .change;
    let w = undo_of(&mut store, &hub, u).unwrap();
    // Landed, and nothing has read that since.
    hub.steward_lands(REMI, w.pr);
    let err = *undo_of(&mut store, &hub, z).unwrap_err().error;
    assert!(
        matches!(err, UndoError::UndoneUndo { change, undo, by }
            if change == z && undo == u && by == w.change),
        "{err}"
    );
    assert_eq!(store.ledger().change(z).unwrap().status(), Kept::Landed);
}

#[test]
fn only_a_landed_change_not_yet_reverted_is_undone() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github();
    let unknown = ChangeId::new().unwrap();
    assert!(matches!(
        undo_of(&mut store, &hub, unknown).map_err(|s| *s.error),
        Err(UndoError::Unknown(c)) if c == unknown
    ));
    let open = ChangeId::new().unwrap();
    store
        .record(Body::Observe {
            change: open,
            repo: repo(),
            pr: 6,
            author: Actor {
                kind: ActorKind::Person,
                login: "TusanHomichi".into(),
                session: None,
            },
        })
        .unwrap();
    assert!(matches!(
        undo_of(&mut store, &hub, open).map_err(|s| *s.error),
        Err(UndoError::NotLanded(c)) if c == open
    ));
    let x = landed(&mut store, Some("tools: a plain sentence"));
    let u = undo_of(&mut store, &hub, x).unwrap();
    let reverted = hub.take_out_as(&"5".repeat(40), TRUNK, CHANGE);
    hub.set_branch(REMI, "main", &reverted);
    land(&mut store, u.change, TRUNK, &reverted);
    assert_eq!(store.ledger().change(x).unwrap().status(), Kept::Reverted);
    assert!(matches!(
        undo_of(&mut store, &hub, x).map_err(|s| *s.error),
        Err(UndoError::Reverted { change, by }) if change == x && by == u.change
    ));
    assert_eq!(pulls(&hub), 1);
    // Its undo undone puts it back, and only undoing that reverts it again.
    let w = undo_of(&mut store, &hub, u.change).unwrap();
    land(
        &mut store,
        w.change,
        "5555555555555555555555555555555555555555",
        "6666666666666666666666666666666666666666",
    );
    assert_eq!(store.ledger().change(x).unwrap().status(), Kept::Landed);
    assert!(matches!(
        undo_of(&mut store, &hub, x).map_err(|s| *s.error),
        Err(UndoError::UndoneUndo { change, undo, by })
            if change == x && undo == u.change && by == w.change
    ));
    assert_eq!(pulls(&hub), 2);
}

#[test]
fn an_undo_is_titled_in_the_style_of_what_it_reverts() {
    for (said, title) in [
        (
            Some("autoland, train: land in order"),
            "autoland, train: undo #5, land in order",
        ),
        (Some("Fix the thing"), "undo: #5, Fix the thing"),
        (None, "undo: #5"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(&dir);
        let x = landed(&mut store, said);
        assert_eq!(super::title(store.ledger().change(x).unwrap()), title);
    }
}

#[test]
fn an_undo_reopened_on_github_is_found_not_made_again() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github();
    let x = landed(&mut store, Some("tools: a plain sentence"));
    let first = undo_of(&mut store, &hub, x).unwrap();
    let read = |store: &mut Store| {
        ingest::pull(store, &hub, &config(), &repo(), first.pr).unwrap();
    };
    hub.close(REMI, first.pr);
    read(&mut store);
    // Reopened since it was read closed.
    hub.reopen(REMI, first.pr);
    let got = undo_of(&mut store, &hub, x).unwrap();
    assert_eq!((got.change, got.how), (first.change, How::Found));
    // And landed since, unread.
    hub.close(REMI, first.pr);
    read(&mut store);
    hub.reopen(REMI, first.pr);
    hub.steward_lands(REMI, first.pr);
    assert!(matches!(
        undo_of(&mut store, &hub, x).map_err(|s| *s.error),
        Err(UndoError::Reverted { change, by }) if change == x && by == first.change
    ));
    assert_eq!(pulls(&hub), 1);
}

#[test]
fn an_undo_s_own_undo_is_read_down_the_tree() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github();
    let x = landed(&mut store, Some("tools: a plain sentence"));
    let u = undo_of(&mut store, &hub, x).unwrap();
    hub.steward_lands(REMI, u.pr);
    ingest::pull(&mut store, &hub, &config(), &repo(), u.pr).unwrap();
    // The undo of the undo, from a run that stopped before recording it,
    // landed: nothing has read either.
    let pr = stopped(&hub, u.change);
    hub.steward_lands(REMI, pr);
    let err = *undo_of(&mut store, &hub, x).unwrap_err().error;
    let w = store.ledger().by_pr(&repo(), pr).unwrap().id;
    assert!(
        matches!(err, UndoError::UndoneUndo { change, undo, by }
            if change == x && undo == u.change && by == w),
        "{err}"
    );
    assert_eq!(store.ledger().change(x).unwrap().status(), Kept::Landed);
}

#[test]
fn a_stopped_run_closed_is_recorded_beside_the_undo_that_reopened() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store(&dir);
    let hub = github();
    let x = landed(&mut store, Some("tools: a plain sentence"));
    let first = undo_of(&mut store, &hub, x).unwrap();
    hub.close(REMI, first.pr);
    ingest::pull(&mut store, &hub, &config(), &repo(), first.pr).unwrap();
    // The next attempt's run stopped before recording its PR, which closed,
    // and then the first reopened.
    let second = stopped_at(&hub, x, 2);
    hub.close(REMI, second);
    hub.reopen(REMI, first.pr);
    let got = undo_of(&mut store, &hub, x).unwrap();
    assert_eq!((got.change, got.how), (first.change, How::Found));
    let closed = store.ledger().by_pr(&repo(), second).unwrap();
    assert_eq!(closed.status(), Kept::Closed);
    assert_eq!(
        store.ledger().change(x).unwrap().undone_by,
        [first.change, closed.id]
    );
    // Recorded once: run again, there's nothing new.
    let again = undo_of(&mut store, &hub, x).unwrap();
    assert_eq!((again.change, again.recorded), (first.change, 0));
    assert_eq!(pulls(&hub), 2);
}

#[test]
fn a_stopped_run_that_merged_after_its_rival_landed_is_trunk_moving_outside() {
    // The next attempt's run stopped before recording its PR, which closed;
    // the first reopened and landed, then the second reopened and merged
    // too. Taken up by `undo` itself, or after a sync read it first, or
    // after the second alone was read: the first is the undo, and the
    // second's merge reverted it again outside Conary.
    for read_first in [None, Some(true), Some(false)] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store(&dir);
        let hub = github();
        let x = landed(&mut store, Some("tools: a plain sentence"));
        let first = undo_of(&mut store, &hub, x).unwrap();
        hub.close(REMI, first.pr);
        ingest::pull(&mut store, &hub, &config(), &repo(), first.pr).unwrap();
        let second = stopped_at(&hub, x, 2);
        hub.close(REMI, second);
        hub.reopen(REMI, first.pr);
        hub.steward_lands(REMI, first.pr);
        hub.reopen(REMI, second);
        let again = hub.steward_lands(REMI, second);
        match read_first {
            Some(true) => _ = ingest::sync(&mut store, &hub, &config(), &repo()).unwrap(),
            Some(false) => _ = ingest::pull(&mut store, &hub, &config(), &repo(), second).unwrap(),
            None => {}
        }
        let err = *undo_of(&mut store, &hub, x).unwrap_err().error;
        assert!(
            matches!(err, UndoError::Reverted { change, by } if change == x && by == first.change),
            "{read_first:?}: {err}"
        );
        let held = store.ledger().by_pr(&repo(), second).unwrap();
        assert_eq!(held.status(), Kept::Closed, "{read_first:?}");
        assert_eq!(
            store.ledger().change(x).unwrap().undone_by,
            [first.change, held.id]
        );
        let outside = store.ledger().outside();
        assert_eq!(outside.len(), 1, "{read_first:?}");
        assert_eq!(outside[0].after.as_str(), again);
        // Taken up again, nothing's new.
        let err = *undo_of(&mut store, &hub, x).unwrap_err().error;
        assert!(matches!(err, UndoError::Reverted { .. }), "{err}");
    }
}

mod group;
mod reason;
