//! Undos as ingest reads them: what they revert, and which of two lands.

use super::*;

/// PR 9 landed as MERGE, with an undo of it recorded on PR `undo`, whose
/// head is HEAD2: open, or merged as REBASED on top.
fn with_undo(store: &mut Store, f: &mut Fake, undo: u64) -> (ChangeId, ChangeId) {
    f.pulls = vec![pr(9, HEAD, "closed", true)];
    let x = pull(store, f, &config(), &repo(), 9).unwrap().change;
    f.pulls.push(pr(undo, HEAD2, "open", false));
    f.statuses.insert(HEAD2.into(), f.statuses[HEAD].clone());
    f.commits
        .insert(REBASED.into(), trunk_commit(REBASED, &[MERGE]));
    f.linked.insert(REBASED.into(), vec![undo]);
    let u = pull(store, f, &config(), &repo(), undo).unwrap().change;
    store
        .record(Body::Undo {
            change: u,
            reverts: vec![x],
            asked: conary_core::op::Asked::Change,
        })
        .unwrap();
    (x, u)
}

fn merged_as(f: &mut Fake, number: u64, sha: &str) {
    let p = f.pulls.iter_mut().find(|p| p["number"] == number).unwrap();
    p["state"] = json!("closed");
    p["merged_at"] = json!("2026-10-27T12:00:00Z");
    p["merge_commit_sha"] = json!(sha);
}

#[test]
fn a_reverted_pr_read_again_is_landed_still() {
    let (_dir, mut store) = open();
    let mut f = fake();
    let (x, u) = with_undo(&mut store, &mut f, 10);
    merged_as(&mut f, 10, REBASED);
    pull(&mut store, &f, &config(), &repo(), 10).unwrap();
    let status = |s: &Store| s.ledger().change(x).unwrap().status();
    assert_eq!(status(&store), Status::Reverted);
    // Its title changed since, so it's read again: no second landing, and
    // no violation for the merge it landed with.
    f.pulls[0]["title"] = json!("autoland: merge only what was checked");
    let got = pull(&mut store, &f, &config(), &repo(), 9).unwrap();
    assert_eq!(got.recorded, 1);
    assert_eq!(status(&store), Status::Reverted);
    assert!(store.ledger().outside().is_empty());
    assert_eq!(store.ledger().change(x).unwrap().reverted_by, Some(u));
    assert!(sync(&mut store, &f, &config(), &repo()).is_ok());
}

/// 9 landed, its first undo, 10, closed, and a second, 11, open in its
/// place: 9's change, the first and the second.
fn two_undos(store: &mut Store, f: &mut Fake) -> [ChangeId; 3] {
    let (x, first) = with_undo(store, f, 10);
    f.pulls[1]["state"] = json!("closed");
    pull(store, f, &config(), &repo(), 10).unwrap();
    f.pulls.push(pr(11, DEV, "open", false));
    let second = pull(store, f, &config(), &repo(), 11).unwrap().change;
    store
        .record(Body::Undo {
            change: second,
            reverts: vec![x],
            asked: conary_core::op::Asked::Change,
        })
        .unwrap();
    [x, first, second]
}

/// [`two_undos`], then GitHub as `then` leaves it, read each way it can be:
/// one PR at a time in either order, a sync and a backfill. Each way's
/// ledger, with 9's change, the first undo and the second.
fn read_every_way(then: impl Fn(&mut Fake)) -> Vec<((tempfile::TempDir, Store), [ChangeId; 3])> {
    fn one(s: &mut Store, f: &Fake, n: u64) {
        pull(s, f, &config(), &repo(), n).unwrap();
    }
    let ways: [fn(&mut Store, &Fake); 4] = [
        |s, f| [10, 11].into_iter().for_each(|n| one(s, f, n)),
        |s, f| [11, 10].into_iter().for_each(|n| one(s, f, n)),
        |s, f| _ = sync(s, f, &config(), &repo()).unwrap(),
        |s, f| _ = backfill(s, f, &config(), &repo()).unwrap(),
    ];
    ways.iter()
        .map(|read| {
            let (dir, mut store) = open();
            let mut f = fake();
            let ids = two_undos(&mut store, &mut f);
            then(&mut f);
            read(&mut store, &f);
            ((dir, store), ids)
        })
        .collect()
}

#[test]
fn an_undo_reopened_beside_another_stays_closed_and_its_merge_is_outside() {
    let (_dir, mut store) = open();
    let mut f = fake();
    let [x, first, second] = two_undos(&mut store, &mut f);
    let status = |s: &Store, c| s.ledger().change(c).unwrap().status();
    // The first reopened on GitHub stays closed here.
    f.pulls[1]["state"] = json!("open");
    pull(&mut store, &f, &config(), &repo(), 10).unwrap();
    assert_eq!(status(&store, first), Status::Closed);
    assert_eq!(store.ledger().undo_of(x), Some(second));
    // Merged anyway, it's trunk moving outside Conary.
    merged_as(&mut f, 10, REBASED);
    pull(&mut store, &f, &config(), &repo(), 10).unwrap();
    assert_eq!(status(&store, first), Status::Closed);
    assert_eq!(status(&store, x), Status::Landed);
    let outside = store.ledger().outside();
    assert_eq!(outside.len(), 1);
    assert_eq!(outside[0].after, sha(REBASED));
    // And it stays outside once the other undo closes, read again any way.
    f.pulls[2]["state"] = json!("closed");
    pull(&mut store, &f, &config(), &repo(), 11).unwrap();
    pull(&mut store, &f, &config(), &repo(), 10).unwrap();
    backfill(&mut store, &f, &config(), &repo()).unwrap();
    assert_eq!(status(&store, first), Status::Closed);
    assert_eq!(status(&store, x), Status::Landed);
    assert_eq!(store.ledger().outside().len(), 1);
}

#[test]
fn an_undo_merged_once_its_rival_had_closed_lands_whichever_is_read_first() {
    // On GitHub the second closed, then the first reopened and landed at
    // noon. Since, the second stayed closed, reopened, or reopened and
    // closed again: a reopening after the landing comes too late, so the
    // ledger holds it closed whatever GitHub shows now.
    let since: [(&str, &[_]); 3] = [
        ("closed", &[("2026-10-27T11:50:00Z", "closed")]),
        (
            "open",
            &[
                ("2026-10-27T11:50:00Z", "closed"),
                ("2026-10-27T12:10:00Z", "reopened"),
            ],
        ),
        (
            "closed",
            &[
                ("2026-10-27T11:50:00Z", "closed"),
                ("2026-10-27T12:10:00Z", "reopened"),
                ("2026-10-27T12:20:00Z", "closed"),
            ],
        ),
    ];
    for (state, turns) in since {
        let got = read_every_way(|f| {
            f.pulls[2]["state"] = json!(state);
            f.turns.insert(11, turns.to_vec());
            f.pulls[1]["state"] = json!("open");
            merged_as(f, 10, REBASED);
        });
        for ((_dir, store), [x, first, second]) in got {
            let status = |c| store.ledger().change(c).unwrap().status();
            assert_eq!(status(second), Status::Closed, "{turns:?}");
            assert_eq!(status(first), Status::Landed, "{turns:?}");
            assert_eq!(store.ledger().change(x).unwrap().reverted_by, Some(first));
            assert!(store.ledger().outside().is_empty(), "{turns:?}");
        }
    }
}

#[test]
fn an_undo_merged_while_its_rival_was_open_is_outside_whatever_closed_after() {
    // The first reopened and landed at noon with the second open on GitHub:
    // never closed before, closed and reopened before, or reopened in that
    // very second, which could be either side. Closing since authorizes
    // nothing, whichever is read first.
    let racing: [&[_]; 3] = [
        &[("2026-10-27T12:30:00Z", "closed")],
        &[
            ("2026-10-27T11:30:00Z", "closed"),
            ("2026-10-27T11:40:00Z", "reopened"),
            ("2026-10-27T12:30:00Z", "closed"),
        ],
        &[
            ("2026-10-27T11:30:00Z", "closed"),
            ("2026-10-27T12:00:00Z", "reopened"),
            ("2026-10-27T12:30:00Z", "closed"),
        ],
    ];
    for turns in racing {
        let got = read_every_way(|f| {
            f.pulls[1]["state"] = json!("open");
            merged_as(f, 10, REBASED);
            f.pulls[2]["state"] = json!("closed");
            f.turns.insert(11, turns.to_vec());
        });
        for ((_dir, store), [x, first, second]) in got {
            let status = |c| store.ledger().change(c).unwrap().status();
            assert_eq!(status(first), Status::Closed, "{turns:?}");
            assert_eq!(status(second), Status::Closed, "{turns:?}");
            assert_eq!(status(x), Status::Landed, "{turns:?}");
            let outside = store.ledger().outside();
            assert_eq!(outside.len(), 1, "{turns:?}");
            assert_eq!(outside[0].after, sha(REBASED));
        }
    }
}

const LATER: &str = "9999999999999999999999999999999999999999";

/// The second undo merged at `at`: onto trunk as LATER, settled, or into
/// another branch.
fn second_merged(f: &mut Fake, at: &str, onto_trunk: bool) {
    let p = &mut f.pulls[2];
    p["state"] = json!("closed");
    p["merged_at"] = json!(at);
    if onto_trunk {
        p["merge_commit_sha"] = json!(LATER);
        f.commits
            .insert(LATER.into(), trunk_commit(LATER, &[REBASED]));
        f.linked.insert(LATER.into(), vec![11]);
        f.statuses.insert(DEV.into(), f.statuses[HEAD].clone());
    } else {
        p["base"]["ref"] = json!("dev");
        p["merge_commit_sha"] = json!(LATER);
    }
}

#[test]
fn an_undo_lands_over_a_rival_that_merged_elsewhere_or_after_it() {
    // The second closed before the first reopened and landed at noon. Then
    // it merged into another branch before that, or reopened and merged
    // onto trunk after: neither was in the first's way, and the second's
    // merge onto trunk is a second revert, outside, whichever is read first.
    for onto_trunk in [false, true] {
        let got = read_every_way(|f| {
            if onto_trunk {
                f.turns.insert(
                    11,
                    vec![
                        ("2026-10-27T11:50:00Z", "closed"),
                        ("2026-10-27T12:10:00Z", "reopened"),
                        ("2026-10-27T12:20:00Z", "closed"),
                    ],
                );
                second_merged(f, "2026-10-27T12:20:00Z", true);
            } else {
                f.turns.insert(11, vec![("2026-10-27T11:50:00Z", "closed")]);
                second_merged(f, "2026-10-27T11:50:00Z", false);
            }
            f.pulls[1]["state"] = json!("open");
            merged_as(f, 10, REBASED);
        });
        for ((_dir, store), [x, first, second]) in got {
            let status = |c| store.ledger().change(c).unwrap().status();
            assert_eq!(status(first), Status::Landed, "{onto_trunk}");
            assert_eq!(status(second), Status::Closed, "{onto_trunk}");
            assert_eq!(store.ledger().change(x).unwrap().reverted_by, Some(first));
            let outside: Vec<_> = store
                .ledger()
                .outside()
                .iter()
                .map(|o| o.after.clone())
                .collect();
            assert_eq!(outside, if onto_trunk { vec![sha(LATER)] } else { vec![] });
        }
    }
}

/// The body `cn undo` gives `x`'s undo, its marker last.
fn marked(x: ChangeId) -> String {
    let marker = crate::undo::Marker {
        reverts: vec![x],
        asked: conary_core::op::Asked::Change,
    };
    format!("Undoes #9.\n\n{}", marker.line())
}

/// 9 landed, and PR `number` from the branch of its `attempt`th undo in
/// `head_repo`, merged as REBASED and settled, with no undo recorded.
fn from_undo_branch(f: &mut Fake, store: &mut Store, number: u64, head_repo: &str) -> ChangeId {
    f.pulls = vec![pr(9, HEAD, "closed", true)];
    let x = pull(store, f, &config(), &repo(), 9).unwrap().change;
    let mut p = pr(number, HEAD2, "closed", true);
    p["merge_commit_sha"] = json!(REBASED);
    p["head"]["ref"] = json!(crate::undo::undo_branch(x, 1));
    p["body"] = json!(marked(x));
    p["head"]["repo"] = json!({"full_name": head_repo, "default_branch": "main"});
    f.pulls.push(p);
    f.statuses.insert(HEAD2.into(), f.statuses[HEAD].clone());
    f.commits
        .insert(REBASED.into(), trunk_commit(REBASED, &[MERGE]));
    f.linked.insert(REBASED.into(), vec![number]);
    x
}

#[test]
fn a_pr_from_an_undo_branch_is_its_undo_whatever_reads_it() {
    let reads: [fn(&mut Store, &Fake); 3] = [
        |s, f| _ = pull(s, f, &config(), &repo(), 10).unwrap(),
        |s, f| _ = sync(s, f, &config(), &repo()).unwrap(),
        |s, f| _ = backfill(s, f, &config(), &repo()).unwrap(),
    ];
    for read in reads {
        let (_dir, mut store) = open();
        let mut f = fake();
        let x = from_undo_branch(&mut f, &mut store, 10, "fieldmouseworks/BURROW");
        read(&mut store, &f);
        let u = store.ledger().by_pr(&repo(), 10).unwrap();
        assert_eq!(u.undo.as_ref().map(|u| u.reverts.clone()), Some(vec![x]));
        assert_eq!(u.status(), Status::Landed);
        assert_eq!(store.ledger().change(x).unwrap().reverted_by, Some(u.id));
    }
    // A fork's branch of that name is anyone's to give: only a PR.
    let (_dir, mut store) = open();
    let mut f = fake();
    let x = from_undo_branch(&mut f, &mut store, 10, "someone/burrow");
    pull(&mut store, &f, &config(), &repo(), 10).unwrap();
    assert!(store.ledger().by_pr(&repo(), 10).unwrap().undo.is_none());
    assert_eq!(store.ledger().change(x).unwrap().status(), Status::Landed);
}

#[test]
fn a_pr_from_an_undo_branch_beside_a_landed_undo_is_recorded_closed_and_its_merge_outside() {
    let (_dir, mut store) = open();
    let mut f = fake();
    let (x, first) = with_undo(&mut store, &mut f, 10);
    merged_as(&mut f, 10, REBASED);
    pull(&mut store, &f, &config(), &repo(), 10).unwrap();
    // A run that stopped before recording its PR, merged since.
    let mut p = pr(11, DEV, "closed", true);
    p["merge_commit_sha"] = json!(LATER);
    p["head"]["ref"] = json!(crate::undo::undo_branch(x, 2));
    p["body"] = json!(marked(x));
    p["head"]["repo"] = json!({"full_name": "FieldmouseWorks/burrow", "default_branch": "main"});
    f.pulls.push(p);
    f.statuses.insert(DEV.into(), f.statuses[HEAD].clone());
    f.commits
        .insert(LATER.into(), trunk_commit(LATER, &[REBASED]));
    f.linked.insert(LATER.into(), vec![11]);
    sync(&mut store, &f, &config(), &repo()).unwrap();
    let second = store.ledger().by_pr(&repo(), 11).unwrap();
    assert_eq!(second.status(), Status::Closed);
    assert_eq!(
        store.ledger().change(x).unwrap().undone_by,
        [first, second.id]
    );
    assert_eq!(store.ledger().change(x).unwrap().reverted_by, Some(first));
    let outside = store.ledger().outside();
    assert_eq!(outside.len(), 1);
    assert_eq!(outside[0].after, sha(LATER));
}
