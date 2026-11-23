//! An empty merge onto trunk alone: the trunk commit it was empty on, for
//! as long as it holds the change out, and never for anything else.

use super::*;

fn train(b: &Book) -> &Train {
    b.ledger.train(&repo()).unwrap()
}

/// `change` leaves as `why`, its candidate built on trunk at `trunk`.
fn leave(b: &mut Book, change: ChangeId, why: Left, trunk: Option<char>) {
    b.ok(Body::Leave {
        change,
        why,
        trunk: trunk.map(commit),
    });
}

#[test]
fn an_empty_merge_onto_trunk_alone_says_which_trunk_until_its_hold_goes() {
    let mut b = Book::default();
    let p = b.change(1, 'a');
    b.join(p);
    leave(&mut b, p, Left::Empty, Some('c'));
    let revision = b.current(p);
    assert_eq!(train(&b).empty_on(p, revision), Some(&commit('c')));

    // Trunk moving lets the hold go, and with it what trunk was.
    b.ok(Body::Outside {
        repo: repo(),
        before: None,
        after: commit('e'),
    });
    assert_eq!(train(&b).empty_on(p, revision), None);

    // A new head isn't the one that was empty.
    let mut b = Book::default();
    let p = b.change(1, 'a');
    b.join(p);
    leave(&mut b, p, Left::Empty, Some('c'));
    b.push(p, 'b');
    assert_eq!(train(&b).empty_on(p, b.current(p)), None);
}

#[test]
fn only_an_empty_merge_holds_a_trunk_it_was_empty_on() {
    // Empty after a change ahead of it: what it does may be that change's,
    // not trunk's.
    let mut b = Book::default();
    let [p, q] = [b.change(1, 'a'), b.change(2, 'b')];
    b.join(p);
    b.join(q);
    leave(&mut b, q, Left::Empty, None);
    assert_eq!(train(&b).held(q, b.current(q)), Some(Held::Empty));
    assert_eq!(train(&b).empty_on(q, b.current(q)), None);

    // Any other way out, given a trunk, keeps none.
    for why in [Left::Conflict, Left::Refused, Left::Unready, Left::Below] {
        let mut b = Book::default();
        let p = b.change(1, 'a');
        b.join(p);
        leave(&mut b, p, why, Some('c'));
        assert_eq!(train(&b).empty_on(p, b.current(p)), None, "{why:?}");
    }
}
