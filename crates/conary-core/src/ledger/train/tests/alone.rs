//! Changes that ride alone, R2 ones: in a candidate of its own, landing
//! only onto its base, and put back alone when one is in below its class.

use super::*;

#[test]
fn a_change_that_joined_alone_rides_in_a_candidate_of_its_own() {
    let mut b = Book::default();
    let a = b.change(1, 'a');
    let r = b.change(2, 'b');
    let c = b.change(3, 'c');
    b.join(a);
    b.join_alone(r);
    b.join(c);
    // The changes before it go without it.
    assert_eq!(b.ledger.next(&repo(), 8).unwrap().changes, [b.member(a)]);
    let with = b.candidate_of(&Plan {
        changes: vec![b.member(a), b.member(r)],
        of: None,
    });
    assert_eq!(b.record(with), Err(Invalid::OutOfTurn));
    let first = b.build(8);
    assert!(!b.ledger.candidate(first).unwrap().alone);
    b.verdict(first, Verdict::Green);
    b.land(a, 'd').unwrap();

    // Then it goes by itself, and the changes after it wait for it.
    assert_eq!(b.ledger.next(&repo(), 8).unwrap().changes, [b.member(r)]);
    let with = b.candidate_of(&Plan {
        changes: vec![b.member(r), b.member(c)],
        of: None,
    });
    assert_eq!(b.record(with), Err(Invalid::OutOfTurn));
    let alone = b.build(8);
    assert!(b.ledger.candidate(alone).unwrap().alone);
    b.verdict(alone, Verdict::Green);
    b.land(r, 'e').unwrap();
    assert_eq!(b.ledger.next(&repo(), 8).unwrap().changes, [b.member(c)]);
}

/// A new join is held to its revision's class, but a log may hold a join
/// below it, from before joins were, and a revision may be classed R2 after
/// it joined: both replay, and the train names them, to reseat alone.
#[test]
fn a_change_in_the_train_below_its_class_replays_and_is_named() {
    let mut b = Book::default();
    let old = b.change(1, 'a');
    let later = b.change(2, 'b');
    let fine = b.change(3, 'c');
    b.classify(old, Class::R2);
    let below = Body::Join {
        change: old,
        revision: b.current(old),
        alone: false,
        raised: Vec::new(),
    };
    assert_eq!(
        b.ledger.admit(&below),
        Err(Invalid::BelowClass(b.current(old)))
    );
    b.write(below);
    b.join(later);
    b.classify(later, Class::R2);
    b.classify(fine, Class::R2);
    b.join_alone(fine);
    let named = [b.member(old), b.member(later)];
    assert_eq!(b.ledger.below_class(&repo()), named);
    let replayed = Ledger::replay(&b.ops).expect("the log replays");
    assert_eq!(replayed.below_class(&repo()), named);
    assert!(Ledger::default().below_class(&repo()).is_empty());

    // Leaving below its class is one op that puts it back in alone, at the
    // back, at the same revision, out of the candidate it was gating in.
    let gating = b.build(8);
    assert_eq!(
        b.ledger.candidate(gating).unwrap().changes,
        [b.member(old), b.member(later)]
    );
    b.ok(Body::Leave {
        change: old,
        why: Left::Below,
    });
    let train = b.ledger.train(&repo()).unwrap();
    assert_eq!(train.in_flight(), None);
    assert_eq!(
        train.joined(),
        [b.member(later), b.member(fine), b.member(old)]
    );
    assert!(train.alone(old));
    assert_eq!(b.ledger.below_class(&repo()), [b.member(later)]);
    let replayed = Ledger::replay(&b.ops).expect("the log replays");
    assert_eq!(replayed.train(&repo()).unwrap().joined(), train.joined());
}

#[test]
fn an_alone_candidate_lands_through_the_train_only_onto_its_base() {
    let mut b = Book::default();
    // Trunk moved after it was built: that landing isn't the candidate's,
    // and it stops there.
    let r = b.change(1, 'a');
    b.join_alone(r);
    let id = b.build(8);
    b.verdict(id, Verdict::Green);
    b.land_on(r, '9', 'd').unwrap();
    assert_eq!(b.landing(r), None);
    assert_eq!(
        b.ledger.candidate(id).unwrap().state,
        State::Broken { by: r, green: true }
    );

    let s = b.change(2, 'b');
    b.join_alone(s);
    let id = b.build(8);
    b.verdict(id, Verdict::Green);
    b.land(s, 'e').unwrap();
    assert_eq!(b.landing(s), Some(id));
    assert_eq!(b.ledger.candidate(id).unwrap().state, State::Landed);
}

/// A change raised by a label recorded since it joined is named to leave,
/// not to go back in alone, even when the raise is `r2`: the steward
/// settled it without the label. One that joined with its raise isn't, and
/// going back in alone keeps what it joined with.
#[test]
fn a_change_raised_since_it_joined_is_named_to_leave_not_reseat() {
    let mut b = Book::default();
    let since = b.change(1, 'a');
    let with = b.change(2, 'b');
    b.join(since);
    let raise = |b: &mut Book, change, class, raised: &[&str]| {
        let revision = b.current(change);
        b.ok(Body::Classify {
            change,
            revision,
            class,
            because: (class == Class::R2).then(|| "labelled r2".into()),
            docs: false,
            policy: format!("{class:?}{raised:?}"),
            raised: raised.iter().map(|l| (*l).to_owned()).collect(),
        });
    };
    raise(&mut b, with, Class::R0, &["review"]);
    let join = |b: &Book, raised: &[&str]| Body::Join {
        change: with,
        revision: b.current(with),
        alone: false,
        raised: raised.iter().map(|l| (*l).to_owned()).collect(),
    };
    assert_eq!(b.ledger.admit(&join(&b, &[])), Err(Invalid::JoinRaised));
    b.ok(join(&b, &["review"]));
    raise(&mut b, since, Class::R2, &["r2"]);
    raise(&mut b, with, Class::R2, &["r2", "review"]);
    assert_eq!(
        b.ledger.raised_since_join(&repo()),
        [b.member(since), b.member(with)]
    );
    assert!(b.ledger.below_class(&repo()).is_empty());

    // Joined with every raise, R2 by the policy: back in alone, and still
    // not raised since it joined.
    let reclassed = b.change(3, 'c');
    raise(&mut b, reclassed, Class::R0, &["review"]);
    b.ok(Body::Join {
        change: reclassed,
        revision: b.current(reclassed),
        alone: false,
        raised: vec!["review".into()],
    });
    let revision = b.current(reclassed);
    b.ok(Body::Classify {
        change: reclassed,
        revision,
        class: Class::R2,
        because: Some("CODEOWNERS".into()),
        docs: false,
        policy: "p2".into(),
        raised: vec!["review".into()],
    });
    assert_eq!(b.ledger.below_class(&repo()), [b.member(reclassed)]);
    b.ok(Body::Leave {
        change: reclassed,
        why: Left::Below,
    });
    let replayed = Ledger::replay(&b.ops).expect("the log replays");
    for ledger in [&b.ledger, &replayed] {
        assert_eq!(
            ledger.raised_since_join(&repo()),
            [b.member(since), b.member(with)]
        );
        assert!(ledger.train(&repo()).unwrap().alone(reclassed));
        assert_eq!(ledger.train(&repo()).unwrap().raised(reclassed), ["review"]);
    }
}
