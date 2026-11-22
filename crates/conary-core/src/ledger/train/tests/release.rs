//! A change red alone let join again by its gate's later pass.

use super::*;

#[test]
fn a_release_frees_what_is_red_alone_and_rewrites_nothing() {
    let mut b = Book::default();
    let [p, q]: [ChangeId; 2] = std::array::from_fn(|i| b.change(i as u64 + 1, 'a'));
    b.join(p);
    b.join(q);
    let whole = b.build(8);
    let release = |candidate| Body::Release { candidate };
    assert_eq!(b.record(release(whole)), Err(Invalid::NotHolding), "gating");
    b.verdict(whole, Verdict::Red);
    assert_eq!(b.record(release(whole)), Err(Invalid::NotHolding), "split");
    let alone = b.build(8);
    b.verdict(alone, Verdict::Red);
    assert_eq!(b.ledger.red_alone(&repo()), [alone]);

    b.ok(release(alone));
    assert_eq!(b.ledger.candidate(alone).unwrap().state, State::Red);
    assert_eq!(b.record(release(alone)), Err(Invalid::NotHolding), "again");
    assert!(b.ledger.red_alone(&repo()).is_empty());
    b.join(p);
    assert_eq!(b.joined(), [q, p]);

    // Red again, only the later candidate holds it.
    let half = b.build(8);
    b.verdict(half, Verdict::Dropped);
    let half = b.build(8);
    b.verdict(half, Verdict::Green);
    b.land(q, 'd').unwrap();
    let later = b.build(8);
    b.verdict(later, Verdict::Red);
    assert_eq!(b.record(release(alone)), Err(Invalid::NotHolding), "stale");
    assert_eq!(b.ledger.red_alone(&repo()), [later]);

    let mut replayed = Ledger::default();
    for op in &b.ops {
        replayed.check(&op.op.body).expect("the log replays");
        replayed.apply(op);
    }
    assert_eq!(replayed.train(&repo()), b.ledger.train(&repo()));

    // A new head needs no release, and takes none.
    b.push(p, 'b');
    assert_eq!(b.record(release(later)), Err(Invalid::NotHolding), "moved");
}

#[test]
fn a_release_is_written_as_its_candidate_alone() {
    let candidate = CandidateId::new().unwrap();
    let body = Body::Release { candidate };
    let json = serde_json::to_value(&body).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"op": "release", "candidate": candidate})
    );
    assert_eq!(serde_json::from_value::<Body>(json).unwrap(), body);
}
