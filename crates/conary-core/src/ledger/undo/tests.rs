use super::*;
use crate::id::{CommitId, OpId};
use crate::op::{Actor, ActorKind, Authority, Op, Repo, Unsigned};

fn remi() -> Repo {
    "FieldmouseWorks/remi".parse().unwrap()
}

fn commit(n: u64) -> CommitId {
    format!("{n:040x}").parse().unwrap()
}

/// A ledger the tests write to as the store does: each op checked, then
/// applied, a millisecond apart.
#[derive(Default)]
struct Book {
    ledger: Ledger,
    n: u64,
}

impl Book {
    fn record(&mut self, body: Body) -> Result<(), Invalid> {
        self.ledger.check(&body)?;
        self.n += 1;
        let op = Op {
            id: OpId::of(&self.n.to_le_bytes()),
            op: Unsigned {
                parent: None,
                at: self.n as i64,
                body,
            },
        };
        self.ledger.apply(&op);
        Ok(())
    }

    fn ok(&mut self, body: Body) {
        self.record(body).expect("the op applies");
    }

    /// An open change in `repo`, by `session` when it's an agent's.
    fn change_in(&mut self, repo: Repo, session: Option<&str>) -> ChangeId {
        let change = ChangeId::new().unwrap();
        self.n += 1;
        let pr = self.n;
        self.ok(Body::Observe {
            change,
            repo,
            pr,
            author: Actor {
                kind: ActorKind::Agent,
                login: "TusanHomichi".into(),
                session: session.map(str::to_owned),
            },
        });
        self.ok(Body::Revision {
            change,
            base: commit(0),
            head: commit(pr),
        });
        change
    }

    fn change(&mut self) -> ChangeId {
        self.change_in(remi(), None)
    }

    fn land(&mut self, change: ChangeId) {
        let revision = self.ledger.change(change).unwrap().current.unwrap();
        self.n += 1;
        self.ok(Body::Land {
            change,
            revision,
            before: commit(self.n),
            after: commit(1000 + self.n),
            by: Authority::Status {
                login: "fieldmouseworks-steward[bot]".into(),
            },
        });
    }

    fn landed(&mut self) -> ChangeId {
        let c = self.change();
        self.land(c);
        c
    }

    fn undo(
        &mut self,
        change: ChangeId,
        reverts: &[ChangeId],
        asked: Asked,
    ) -> Result<(), Invalid> {
        self.record(Body::Undo {
            change,
            reverts: reverts.to_vec(),
            asked,
        })
    }

    /// An undo of `of` alone, recorded, and open.
    fn undo_of(&mut self, of: ChangeId) -> ChangeId {
        let u = self.change();
        self.undo(u, &[of], Asked::Change)
            .expect("the undo applies");
        u
    }

    fn status(&self, c: ChangeId) -> Status {
        self.ledger.change(c).unwrap().status()
    }
}

#[test]
fn a_change_is_reverted_once_its_undo_lands_and_landed_again_once_that_is_undone() {
    let mut b = Book::default();
    let x = b.landed();
    let u = b.undo_of(x);
    // Open, the undo reverts nothing yet.
    assert_eq!(b.status(x), Status::Landed);
    let got = b.ledger.change(x).unwrap();
    assert_eq!((got.undone_by.clone(), got.reverted_by), (vec![u], None));
    assert_eq!(
        b.ledger
            .change(u)
            .unwrap()
            .undo
            .as_ref()
            .map(|d| d.reverts.clone()),
        Some(vec![x])
    );
    b.land(u);
    assert_eq!(b.status(x), Status::Reverted);
    assert_eq!(b.ledger.change(x).unwrap().reverted_by, Some(u));
    // A reverted change has landed: it doesn't land or close again.
    assert_eq!(
        b.record(Body::Close { change: x }),
        Err(Invalid::AlreadyLanded)
    );
    // The undo undone, the change is back; that undone, it's reverted again.
    let redo = b.undo_of(u);
    b.land(redo);
    assert_eq!(
        (b.status(u), b.status(x)),
        (Status::Reverted, Status::Landed)
    );
    assert_eq!(b.ledger.change(x).unwrap().reverted_by, None);
    let again = b.undo_of(redo);
    b.land(again);
    assert_eq!(
        (b.status(redo), b.status(u), b.status(x)),
        (Status::Reverted, Status::Landed, Status::Reverted)
    );
    assert_eq!(b.ledger.change(x).unwrap().reverted_by, Some(u));
}

#[test]
fn an_undo_recorded_after_its_pr_landed_is_in_effect_at_once() {
    let mut b = Book::default();
    let x = b.landed();
    let u = b.landed();
    b.undo(u, &[x], Asked::Change).unwrap();
    assert_eq!(b.status(x), Status::Reverted);
}

#[test]
fn an_undo_reverts_only_landed_changes_of_its_repo_each_once() {
    let mut b = Book::default();
    let x = b.landed();
    let u = b.change();
    assert_eq!(b.undo(u, &[], Asked::Change), Err(Invalid::NothingToUndo));
    assert_eq!(b.undo(u, &[u], Asked::Change), Err(Invalid::UndoRepeats(u)));
    let y = b.landed();
    // One change asked for, one reverted; any more, each once.
    assert_eq!(b.undo(u, &[x, y], Asked::Change), Err(Invalid::NotAsked(y)));
    let since = Asked::Since { since: 0 };
    assert_eq!(b.undo(u, &[x, y, x], since), Err(Invalid::UndoRepeats(x)));
    let open = b.change();
    assert_eq!(
        b.undo(u, &[open], Asked::Change),
        Err(Invalid::NotLandedYet(open))
    );
    let elsewhere = b.change_in("FieldmouseWorks/burrow".parse().unwrap(), None);
    b.land(elsewhere);
    assert_eq!(
        b.undo(u, &[elsewhere], Asked::Change),
        Err(Invalid::OtherRepo(elsewhere))
    );
    let unknown = ChangeId::new().unwrap();
    assert_eq!(
        b.undo(u, &[unknown], Asked::Change),
        Err(Invalid::UnknownChange(unknown))
    );
    // Nothing refused was written.
    assert!(b.ledger.change(x).unwrap().undone_by.is_empty());
    // Recorded once: the same again is nothing new, and another is refused.
    b.undo(u, &[x], Asked::Change).unwrap();
    assert_eq!(b.undo(u, &[x], Asked::Change), Err(Invalid::Duplicate));
    assert_eq!(b.undo(u, &[y], Asked::Change), Err(Invalid::AlreadyUndo));
}

#[test]
fn a_change_takes_another_undo_only_once_its_last_closed_without_landing() {
    let mut b = Book::default();
    let x = b.landed();
    let first = b.undo_of(x);
    let second = b.change();
    let taken = Err(Invalid::AlreadyUndone {
        change: x,
        by: first,
    });
    assert_eq!(b.undo(second, &[x], Asked::Change), taken);
    // Landed, it's in effect: still taken.
    b.land(first);
    assert_eq!(b.undo(second, &[x], Asked::Change), taken);
    // Undone, it's still the one: the change goes back out by undoing its
    // undo's undo, never by a second undo that could stand beside the first.
    let redo = b.undo_of(first);
    b.land(redo);
    assert_eq!(b.status(x), Status::Landed);
    assert_eq!(b.undo(second, &[x], Asked::Change), taken);
    let again = b.undo_of(redo);
    b.land(again);
    assert_eq!(b.status(x), Status::Reverted);
    // Closed without landing, it makes way.
    let y = b.landed();
    let dropped = b.undo_of(y);
    b.ok(Body::Close { change: dropped });
    let third = b.change();
    b.undo(third, &[y], Asked::Change).unwrap();
    // And it can't reopen beside the one that took its place.
    assert_eq!(
        b.record(Body::Reopen { change: dropped }),
        Err(Invalid::AlreadyUndone {
            change: y,
            by: third
        })
    );
    b.ok(Body::Close { change: third });
    b.ok(Body::Reopen { change: dropped });
    // One closed without landing reverts nothing, so it's recorded beside
    // the one in its place, and can't reopen while that one is open.
    let late = b.change();
    b.ok(Body::Close { change: late });
    b.undo(late, &[y], Asked::Change).unwrap();
    assert_eq!(b.status(y), Status::Landed);
    assert_eq!(
        b.record(Body::Reopen { change: late }),
        Err(Invalid::AlreadyUndone {
            change: y,
            by: dropped
        })
    );
}

#[test]
fn undos_that_revert_each_other_round_a_cycle_are_refused() {
    let mut b = Book::default();
    let (x, y, z) = (b.landed(), b.landed(), b.landed());
    // Each landed already, each recorded as the next one's undo.
    b.undo(x, &[y], Asked::Change).unwrap();
    b.undo(y, &[z], Asked::Change).unwrap();
    assert_eq!(b.undo(z, &[x], Asked::Change), Err(Invalid::UndoCycle(x)));
    // Through an undo of several too.
    let w = b.landed();
    let since = Asked::Since { since: 0 };
    assert_eq!(b.undo(z, &[w, x], since), Err(Invalid::UndoCycle(x)));
    // Down a chain without a cycle, it's an undo like any other: x reverts
    // y, so y's undo of z is out of effect, and z's of w stands.
    b.undo(z, &[w], Asked::Change).unwrap();
    assert_eq!(
        (b.status(w), b.status(z), b.status(y), b.status(x)),
        (
            Status::Reverted,
            Status::Landed,
            Status::Reverted,
            Status::Landed
        )
    );
}

#[test]
fn an_undo_asked_for_a_session_or_a_time_reverts_only_what_that_names() {
    let mut b = Book::default();
    let mine = b.change_in(remi(), Some("https://claude.ai/code/session_a"));
    b.land(mine);
    let theirs = b.change_in(remi(), Some("https://claude.ai/code/session_b"));
    b.land(theirs);
    let person = b.landed();
    let session = |s: &str| Asked::Actor {
        session: format!("https://claude.ai/code/session_{s}"),
    };
    let u = b.change();
    assert_eq!(
        b.undo(u, &[mine, theirs], session("a")),
        Err(Invalid::NotAsked(theirs))
    );
    assert_eq!(
        b.undo(u, &[person], session("a")),
        Err(Invalid::NotAsked(person))
    );
    // A time names what landed after it, by its landing's op.
    let at = b
        .ledger
        .change(theirs)
        .unwrap()
        .landing
        .as_ref()
        .unwrap()
        .at;
    let since = Asked::Since { since: at };
    assert_eq!(
        b.undo(u, &[theirs, person], since.clone()),
        Err(Invalid::NotAsked(theirs))
    );
    b.undo(u, &[person], since).unwrap();
    let v = b.change();
    b.undo(v, &[mine], session("a")).unwrap();
    b.land(v);
    assert_eq!(
        (b.status(mine), b.status(theirs)),
        (Status::Reverted, Status::Landed)
    );
}

#[test]
fn an_undo_is_written_as_the_log_keeps_it() {
    let (change, x) = (ChangeId::new().unwrap(), ChangeId::new().unwrap());
    for (asked, said) in [
        (Asked::Change, serde_json::json!({"by": "change"})),
        (
            Asked::Actor {
                session: "s".into(),
            },
            serde_json::json!({"by": "actor", "session": "s"}),
        ),
        (
            Asked::Since { since: 7 },
            serde_json::json!({"by": "since", "since": 7}),
        ),
    ] {
        let body = Body::Undo {
            change,
            reverts: vec![x],
            asked,
        };
        let want = serde_json::json!({
            "op": "undo", "change": change.to_string(), "reverts": [x.to_string()], "asked": said,
        });
        assert_eq!(serde_json::to_value(&body).unwrap(), want);
        assert_eq!(serde_json::from_value::<Body>(want).unwrap(), body);
    }
}
