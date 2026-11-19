use super::*;
use crate::id::{CommitId, OpId};
use crate::op::{Actor, ActorKind, Body, Op, Unsigned};

fn commit(n: u64) -> CommitId {
    format!("{n:040x}").parse().unwrap()
}

/// A ledger written to as the store does: each op admitted, then applied.
#[derive(Default)]
struct Book {
    ledger: Ledger,
    n: u64,
}

impl Book {
    fn record(&mut self, body: Body) -> Result<(), Invalid> {
        self.ledger.admit(&body)?;
        self.n += 1;
        self.ledger.apply(&Op {
            id: OpId::of(&self.n.to_le_bytes()),
            op: Unsigned {
                parent: None,
                at: self.n as i64,
                body,
            },
        });
        Ok(())
    }

    /// An open change at one revision.
    fn change(&mut self) -> (ChangeId, RevisionId) {
        let change = ChangeId::new().unwrap();
        self.record(Body::Observe {
            change,
            repo: "FieldmouseWorks/remi".parse().unwrap(),
            pr: 3,
            author: Actor {
                kind: ActorKind::Agent,
                login: "TusanHomichi".into(),
                session: None,
            },
        })
        .unwrap();
        self.record(Body::Revision {
            change,
            base: commit(0),
            head: commit(1),
        })
        .unwrap();
        (change, RevisionId::new(&commit(0), &commit(1)))
    }

    fn degraded(&self, change: ChangeId, revision: RevisionId) -> Vec<&Degradation> {
        self.ledger.change(change).unwrap().degraded(revision).collect()
    }
}

fn degrade(change: ChangeId, revision: RevisionId, what: Degraded, why: &str) -> Body {
    Body::Degraded {
        change,
        revision,
        what,
        why: why.into(),
    }
}

#[test]
fn a_degradation_is_recorded_once_while_it_lasts_and_cleared_by_a_later_op() {
    let mut book = Book::default();
    let (change, revision) = book.change();
    let record = || degrade(change, revision, Degraded::Record, "not JSON");
    book.record(record()).unwrap();
    // Every pass sees it again; the ledger holds it once.
    assert_eq!(book.record(record()), Err(Invalid::Duplicate));
    assert_eq!(book.degraded(change, revision).len(), 1);
    // Another path of the same revision is another degradation.
    let lookup = Degraded::Lookup {
        check: "gate".into(),
    };
    book.record(degrade(change, revision, lookup.clone(), "GitHub: 502"))
        .unwrap();
    book.record(Body::Restored {
        change,
        revision,
        what: Degraded::Record,
    })
    .unwrap();
    assert_eq!(
        book.degraded(change, revision)
            .into_iter()
            .map(|d| &d.what)
            .collect::<Vec<_>>(),
        [&lookup]
    );
    // Nothing is removed: the cleared one is still the change's.
    let all = &book.ledger.change(change).unwrap().degradations;
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].what, Degraded::Record);
    assert!(all[0].restored.is_some());
    // A restore of what isn't degraded is refused, and so is a second one.
    assert_eq!(
        book.record(Body::Restored {
            change,
            revision,
            what: Degraded::Record,
        }),
        Err(Invalid::NotDegraded)
    );
    // Once cleared, the same path can degrade again.
    book.record(record()).unwrap();
    assert_eq!(book.degraded(change, revision).len(), 2);
}

#[test]
fn a_degradation_names_a_known_revision_and_a_bounded_why() {
    let mut book = Book::default();
    let (change, revision) = book.change();
    let other = RevisionId::new(&commit(0), &commit(9));
    assert_eq!(
        book.record(degrade(change, other, Degraded::Record, "not JSON")),
        Err(Invalid::UnknownRevision(other))
    );
    for why in [" ", &"x".repeat(crate::op::DEGRADED_WHY + 1)] {
        assert_eq!(
            book.record(degrade(change, revision, Degraded::Record, why)),
            Err(Invalid::DegradedWhy)
        );
    }
    assert_eq!(
        book.record(Body::Restored {
            change,
            revision,
            what: Degraded::Record,
        }),
        Err(Invalid::NotDegraded)
    );
    assert!(book.degraded(change, revision).is_empty());
}
