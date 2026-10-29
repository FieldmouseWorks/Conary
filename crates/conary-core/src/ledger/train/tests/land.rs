//! Landing through a candidate, in order, and what stops it.

use super::*;

#[test]
fn a_green_candidates_changes_land_through_it_and_trunk_ends_on_its_tree() {
    let mut b = Book::default();
    let a = b.change(1, 'a');
    let c = b.change(2, 'b');
    b.join(a);
    b.join(c);
    let id = b.build(8);
    assert_eq!(b.state(id), State::Gating);
    assert_eq!(
        b.record(Body::Finish {
            candidate: id,
            tree: tree('7')
        }),
        Err(Invalid::NotLanded)
    );
    b.verdict(id, Verdict::Green);
    assert_eq!(
        b.record(Body::Verdict {
            candidate: id,
            verdict: Verdict::Red
        }),
        Err(Invalid::NotGating)
    );

    // A change that joins meanwhile waits for the next candidate.
    let late = b.change(3, 'c');
    b.join(late);
    assert_eq!(b.next(8), None);

    b.land(a, 'd').unwrap();
    assert_eq!(b.landing(a), Some(id));
    assert_eq!(b.state(id), State::Green);
    assert_eq!(b.ledger.train(&repo()).unwrap().in_flight(), Some(id));
    b.land(c, 'e').unwrap();
    assert_eq!(b.landing(c), Some(id));
    assert_eq!(b.state(id), State::Landed);
    assert_eq!(b.next(8).unwrap(), [late]);
    assert!(!b.ledger.candidate(id).unwrap().same_tree());

    b.ok(Body::Finish {
        candidate: id,
        tree: tree('7'),
    });
    assert!(b.ledger.candidate(id).unwrap().same_tree());
    assert_eq!(b.joined(), [late]);
}

#[test]
fn trunk_ending_on_another_tree_isnt_the_candidates() {
    let mut b = Book::default();
    let a = b.change(1, 'a');
    b.join(a);
    let id = b.build(8);
    b.verdict(id, Verdict::Green);
    b.land(a, 'd').unwrap();
    b.ok(Body::Finish {
        candidate: id,
        tree: tree('8'),
    });
    assert!(!b.ledger.candidate(id).unwrap().same_tree());
}

#[test]
fn a_landing_before_the_gate_says_isnt_the_candidates_and_stops_it() {
    let mut b = Book::default();
    let a = b.change(1, 'a');
    let c = b.change(2, 'b');
    b.join(a);
    b.join(c);
    let id = b.build(8);
    b.land(c, 'd').unwrap();
    assert_eq!(b.landing(c), None);
    assert_eq!(
        b.state(id),
        State::Broken {
            by: c,
            green: false
        }
    );
    assert_eq!(
        b.record(Body::Verdict {
            candidate: id,
            verdict: Verdict::Green
        }),
        Err(Invalid::NotGating)
    );
    // The rest wait again, where they joined.
    assert_eq!(b.next(8).unwrap(), [a]);
}

#[test]
fn a_green_candidates_changes_land_in_its_order_or_stop_it() {
    let mut b = Book::default();
    let [p, q, r]: [ChangeId; 3] = std::array::from_fn(|i| b.change(i as u64 + 1, 'a'));
    for c in [p, q, r] {
        b.join(c);
    }

    // Out of order from the first.
    let id = b.build(8);
    b.verdict(id, Verdict::Green);
    b.land(q, 'd').unwrap();
    assert_eq!(b.landing(q), None);
    assert_eq!(b.state(id), State::Broken { by: q, green: true });
    assert_eq!(
        b.record(Body::Finish {
            candidate: id,
            tree: tree('7')
        }),
        Err(Invalid::NotLanded)
    );
    assert_eq!(b.next(8).unwrap(), [p, r]);

    // Out of order after the first landed through it.
    let s = b.change(4, 'b');
    b.join(s);
    let id = b.build(8);
    assert_eq!(b.changes(id), [p, r, s]);
    b.verdict(id, Verdict::Green);
    b.land(p, 'e').unwrap();
    assert_eq!(b.landing(p), Some(id));
    b.land(s, 'f').unwrap();
    assert_eq!(b.landing(s), None);
    assert_eq!(b.state(id), State::Broken { by: s, green: true });
    assert_eq!(b.next(8).unwrap(), [r]);

    // A half's rest goes back as what's left of the half.
    let [t, u, v, w]: [ChangeId; 4] = std::array::from_fn(|i| b.change(i as u64 + 5, 'a'));
    for c in [t, u, v, w] {
        b.join(c);
    }
    let whole = b.build(8);
    assert_eq!(b.changes(whole), [r, t, u, v, w]);
    b.verdict(whole, Verdict::Red);
    let older = b.build(8);
    assert_eq!(b.changes(older), [r, t, u]);
    b.verdict(older, Verdict::Green);
    b.land(u, '9').unwrap();
    assert_eq!(b.state(older), State::Broken { by: u, green: true });
    assert_eq!(b.next(8).unwrap(), [r, t]);
    assert_eq!(b.ledger.next(&repo(), 8).unwrap().of, Some(whole));
}

#[test]
fn a_landing_a_stopped_candidate_had_open_lands_through_it() {
    // A green candidate stopped by its second change's new head: its first,
    // landing after, came through it, since GitHub took that merge before
    // it stopped. Not a change that left since, nor one first in a
    // candidate stopped while gating, nor once another candidate is in
    // flight.
    let mut b = Book::default();
    let [p, q, r]: [ChangeId; 3] = std::array::from_fn(|i| b.change(i as u64 + 1, 'a'));
    for c in [p, q, r] {
        b.join(c);
    }
    let id = b.build(8);
    b.verdict(id, Verdict::Green);
    b.push(q, 'b');
    assert_eq!(b.state(id), State::Broken { by: q, green: true });
    b.land(p, 'd').unwrap();
    assert_eq!(b.landing(p), Some(id));
    assert_eq!(b.ledger.candidate(id).unwrap().landed, [p]);
    assert_eq!(b.joined(), [r]);

    // Left since.
    let [s, t]: [ChangeId; 2] = std::array::from_fn(|i| b.change(i as u64 + 4, 'a'));
    b.ok(Body::Leave {
        change: r,
        why: Left::Unready,
    });
    for c in [s, t] {
        b.join(c);
    }
    let id = b.build(8);
    b.verdict(id, Verdict::Green);
    b.push(t, 'b');
    b.ok(Body::Leave {
        change: s,
        why: Left::Unready,
    });
    b.land(s, 'e').unwrap();
    assert_eq!(b.landing(s), None);

    // Stopped while gating.
    let [u, v]: [ChangeId; 2] = std::array::from_fn(|i| b.change(i as u64 + 6, 'a'));
    for c in [u, v] {
        b.join(c);
    }
    let id = b.build(8);
    b.push(v, 'b');
    assert_eq!(
        b.state(id),
        State::Broken {
            by: v,
            green: false
        }
    );
    b.land(u, 'f').unwrap();
    assert_eq!(b.landing(u), None);

    // Another in flight.
    let [w, x]: [ChangeId; 2] = std::array::from_fn(|i| b.change(i as u64 + 8, 'a'));
    for c in [w, x] {
        b.join(c);
    }
    let id = b.build(8);
    b.verdict(id, Verdict::Green);
    b.push(x, 'b');
    let next = b.build(8);
    assert_eq!(b.changes(next), [w]);
    b.land(w, '9').unwrap();
    assert_eq!(b.landing(w), None);
}
