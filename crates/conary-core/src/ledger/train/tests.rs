use super::*;
use crate::id::OpId;
use crate::op::{Actor, ActorKind, Authority, Left, Op, Unsigned};

fn repo() -> Repo {
    "FieldmouseWorks/remi".parse().unwrap()
}

fn commit(c: char) -> CommitId {
    c.to_string().repeat(40).parse().unwrap()
}

fn tree(c: char) -> TreeId {
    c.to_string().repeat(40).parse().unwrap()
}

/// A ledger the tests write to as the store does: each op checked, then
/// applied.
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

    /// A change on PR `pr`, at a revision whose head is `head`.
    fn change(&mut self, pr: u64, head: char) -> ChangeId {
        let change = ChangeId::new().unwrap();
        self.ok(Body::Observe {
            change,
            repo: repo(),
            pr,
            author: Actor {
                kind: ActorKind::Agent,
                login: "TusanHomichi".into(),
                session: None,
            },
        });
        self.push(change, head);
        change
    }

    /// A new head on the change, or an old one again.
    fn push(&mut self, change: ChangeId, head: char) -> RevisionId {
        self.ok(Body::Revision {
            change,
            base: commit('0'),
            head: commit(head),
        });
        self.current(change)
    }

    fn current(&self, change: ChangeId) -> RevisionId {
        self.ledger.change(change).unwrap().current.unwrap()
    }

    fn member(&self, change: ChangeId) -> Member {
        Member {
            change,
            revision: self.current(change),
        }
    }

    fn join(&mut self, change: ChangeId) {
        let revision = self.current(change);
        self.ok(Body::Join {
            change,
            revision,
            alone: false,
        });
    }

    /// Joins in a candidate of its own, as an R2 change does.
    fn join_alone(&mut self, change: ChangeId) {
        let revision = self.current(change);
        self.ok(Body::Join {
            change,
            revision,
            alone: true,
        });
    }

    fn candidate_of(&self, plan: &Plan) -> Body {
        Body::Candidate {
            candidate: CandidateId::new().unwrap(),
            repo: repo(),
            trunk: "main".into(),
            base: commit('0'),
            changes: plan.changes.clone(),
            commit: commit('c'),
            tree: tree('7'),
            of: plan.of,
        }
    }

    /// Builds the train's next candidate of at most `size`, as `cn train`
    /// would.
    fn build(&mut self, size: usize) -> CandidateId {
        let plan = self
            .ledger
            .next(&repo(), size)
            .expect("a candidate to build");
        let body = self.candidate_of(&plan);
        let Body::Candidate { candidate, .. } = body else {
            unreachable!()
        };
        self.ok(body);
        candidate
    }

    fn verdict(&mut self, candidate: CandidateId, verdict: Verdict) {
        self.ok(Body::Verdict { candidate, verdict });
    }

    fn land(&mut self, change: ChangeId, after: char) -> Result<(), Invalid> {
        self.land_on(change, '0', after)
    }

    /// Lands the change onto trunk at `before`: `'0'` is every candidate's
    /// base.
    fn land_on(&mut self, change: ChangeId, before: char, after: char) -> Result<(), Invalid> {
        let revision = self.current(change);
        self.record(Body::Land {
            change,
            revision,
            before: commit(before),
            after: commit(after),
            by: Authority::Status {
                login: "fieldmouseworks-steward[bot]".into(),
            },
        })
    }

    fn landing(&self, change: ChangeId) -> Option<CandidateId> {
        self.ledger
            .change(change)
            .unwrap()
            .landing
            .as_ref()
            .unwrap()
            .candidate
    }

    fn state(&self, candidate: CandidateId) -> State {
        self.ledger.candidate(candidate).unwrap().state.clone()
    }

    fn changes(&self, candidate: CandidateId) -> Vec<ChangeId> {
        let c = self.ledger.candidate(candidate).unwrap();
        c.changes.iter().map(|m| m.change).collect()
    }

    /// The changes the next candidate would hold.
    fn next(&self, size: usize) -> Option<Vec<ChangeId>> {
        let plan = self.ledger.next(&repo(), size)?;
        Some(plan.changes.iter().map(|m| m.change).collect())
    }

    fn joined(&self) -> Vec<ChangeId> {
        self.ledger
            .train(&repo())
            .map(|t| t.joined().iter().map(|m| m.change).collect())
            .unwrap_or_default()
    }
}

#[test]
fn a_change_joins_once_and_only_at_its_head_now() {
    let mut b = Book::default();
    let a = b.change(1, 'a');
    let old = b.current(a);
    let unknown = RevisionId::new(&commit('0'), &commit('f'));
    assert_eq!(
        b.record(Body::Join {
            change: a,
            revision: unknown,
            alone: false
        }),
        Err(Invalid::UnknownRevision(unknown))
    );
    let new = b.push(a, 'b');
    assert_eq!(
        b.record(Body::Join {
            change: a,
            revision: old,
            alone: false
        }),
        Err(Invalid::NotCurrent(old))
    );
    b.join(a);
    assert_eq!(
        b.record(Body::Join {
            change: a,
            revision: new,
            alone: false
        }),
        Err(Invalid::Duplicate)
    );

    let closed = b.change(3, 'd');
    b.ok(Body::Close { change: closed });
    assert_eq!(
        b.record(Body::Join {
            change: closed,
            revision: b.current(closed),
            alone: false
        }),
        Err(Invalid::Closed)
    );
}

#[test]
fn a_head_pushed_back_to_an_older_one_takes_the_change_out_and_rejoins_at_the_back() {
    let mut b = Book::default();
    let a = b.change(1, 'a');
    let old = b.current(a);
    let new = b.push(a, 'b');
    b.join(a);
    let c = b.change(2, 'c');
    b.join(c);

    // Back to the head it had: the revision it already was, and its head
    // now, recorded once more and no more than that.
    assert_eq!(b.push(a, 'a'), old);
    assert_eq!(b.ledger.change(a).unwrap().revisions.len(), 2);
    assert_eq!(
        b.record(Body::Revision {
            change: a,
            base: commit('0'),
            head: commit('a'),
        }),
        Err(Invalid::Duplicate)
    );
    assert_eq!(b.joined(), [c]);
    assert_eq!(
        b.record(Body::Join {
            change: a,
            revision: new,
            alone: false
        }),
        Err(Invalid::NotCurrent(new))
    );
    b.join(a);
    assert_eq!(b.joined(), [c, a]);
    assert_eq!(b.ledger.train(&repo()).unwrap().joined()[1].revision, old);

    // In a candidate in flight, gating or green, it stops the candidate.
    let id = b.build(8);
    assert_eq!(b.push(a, 'b'), new);
    assert_eq!(
        b.state(id),
        State::Broken {
            by: a,
            green: false
        }
    );
    assert_eq!(b.next(8).unwrap(), [c]);
    b.join(a);
    let id = b.build(8);
    b.verdict(id, Verdict::Green);
    // Seen again at its head now, it's nothing new.
    assert_eq!(
        b.record(Body::Revision {
            change: c,
            base: commit('0'),
            head: commit('c'),
        }),
        Err(Invalid::Duplicate)
    );
    assert_eq!(b.state(id), State::Green);
    b.push(c, 'e');
    b.push(c, 'c');
    assert_eq!(b.state(id), State::Broken { by: c, green: true });
    assert_eq!(b.next(8).unwrap(), [a]);
}

#[test]
fn the_next_candidate_is_the_changes_waiting_in_the_order_they_joined() {
    let mut b = Book::default();
    assert_eq!(b.next(8), None);
    let changes: Vec<ChangeId> = (1..=5).map(|pr| b.change(pr, 'a')).collect();
    for c in changes.iter().rev() {
        b.join(*c);
    }
    let joined: Vec<ChangeId> = changes.iter().rev().copied().collect();
    assert_eq!(b.next(8).unwrap(), joined);
    assert_eq!(b.next(2).unwrap(), joined[..2]);
    assert_eq!(b.ledger.next(&repo(), 8).unwrap().of, None);
}

#[test]
fn a_candidate_out_of_turn_is_refused() {
    let mut b = Book::default();
    let a = b.change(1, 'a');
    let c = b.change(2, 'b');
    let late = b.change(3, 'c');
    b.join(a);
    b.join(c);
    let skips_a = Plan {
        changes: vec![b.member(c)],
        of: None,
    };
    assert_eq!(b.record(b.candidate_of(&skips_a)), Err(Invalid::OutOfTurn));
    let not_joined = Plan {
        changes: vec![b.member(a), b.member(late)],
        of: None,
    };
    assert_eq!(
        b.record(b.candidate_of(&not_joined)),
        Err(Invalid::OutOfTurn)
    );
    let empty = Plan {
        changes: vec![],
        of: None,
    };
    assert_eq!(b.record(b.candidate_of(&empty)), Err(Invalid::OutOfTurn));
    let as_a_half = Plan {
        changes: vec![b.member(a)],
        of: Some(CandidateId::new().unwrap()),
    };
    assert_eq!(
        b.record(b.candidate_of(&as_a_half)),
        Err(Invalid::OutOfTurn)
    );

    // Its first change alone is in turn, and then the train is busy.
    let first = Plan {
        changes: vec![b.member(a)],
        of: None,
    };
    let body = b.candidate_of(&first);
    b.ok(body.clone());
    assert_eq!(b.record(body), Err(Invalid::Duplicate));
    let rest = Plan {
        changes: vec![b.member(c)],
        of: None,
    };
    assert_eq!(b.record(b.candidate_of(&rest)), Err(Invalid::TrainBusy));
    assert_eq!(b.next(8), None);
}

#[test]
fn a_new_head_or_a_close_takes_a_change_out() {
    let mut b = Book::default();
    let a = b.change(1, 'a');
    let c = b.change(2, 'b');
    let d = b.change(3, 'c');
    for x in [a, c, d] {
        b.join(x);
    }
    b.push(a, 'e');
    b.ok(Body::Close { change: d });
    assert_eq!(b.joined(), [c]);
    assert_eq!(
        b.record(Body::Leave {
            change: a,
            why: Left::Unready
        }),
        Err(Invalid::NotInTrain)
    );

    // One in a candidate in flight stops it, and the rest wait again.
    b.join(a);
    let id = b.build(8);
    assert_eq!(b.changes(id), [c, a]);
    b.push(c, 'f');
    assert_eq!(
        b.state(id),
        State::Broken {
            by: c,
            green: false
        }
    );
    assert_eq!(b.next(8).unwrap(), [a]);
}

#[test]
fn a_refused_merge_puts_the_changes_after_it_back() {
    let mut b = Book::default();
    let changes: Vec<ChangeId> = (1..=3).map(|pr| b.change(pr, 'a')).collect();
    for c in &changes {
        b.join(*c);
    }
    let id = b.build(8);
    b.verdict(id, Verdict::Green);
    b.land(changes[0], 'd').unwrap();
    b.ok(Body::Leave {
        change: changes[1],
        why: Left::Refused,
    });
    assert_eq!(
        b.state(id),
        State::Broken {
            by: changes[1],
            green: true
        }
    );
    assert_eq!(b.landing(changes[0]), Some(id));
    assert_eq!(b.next(8).unwrap(), [changes[2]]);
    // A landing after the candidate stopped isn't its.
    b.land(changes[2], 'e').unwrap();
    assert_eq!(b.landing(changes[2]), None);
}

#[test]
fn red_splits_older_half_first_until_the_red_change_leaves() {
    let mut b = Book::default();
    let [p, q, r, s]: [ChangeId; 4] = std::array::from_fn(|i| b.change(i as u64 + 1, 'a'));
    for c in [p, q, r, s] {
        b.join(c);
    }
    let whole = b.build(8);
    b.verdict(whole, Verdict::Red);

    // Each half goes alone, older first, ahead of a change that joined since.
    let late = b.change(5, 'b');
    b.join(late);
    assert_eq!(b.next(8).unwrap(), [p, q]);
    assert_eq!(b.ledger.next(&repo(), 8).unwrap().of, Some(whole));
    let older = b.build(8);
    b.verdict(older, Verdict::Red);
    assert_eq!(b.next(8).unwrap(), [p]);
    let alone = b.build(8);
    b.verdict(alone, Verdict::Red);
    assert!(!b.joined().contains(&p), "red alone, p leaves");

    let next = b.build(8);
    assert_eq!(b.changes(next), [q]);
    b.verdict(next, Verdict::Green);
    b.land(q, 'd').unwrap();
    let newer = b.build(8);
    assert_eq!(b.changes(newer), [r, s]);
    assert_eq!(b.ledger.candidate(newer).unwrap().of, Some(whole));
    b.verdict(newer, Verdict::Green);
    b.land(r, 'e').unwrap();
    b.land(s, 'f').unwrap();
    assert_eq!(b.next(8).unwrap(), [late]);
}

#[test]
fn a_half_is_built_whole_and_without_changes_that_left() {
    let mut b = Book::default();
    let [p, q, r]: [ChangeId; 3] = std::array::from_fn(|i| b.change(i as u64 + 1, 'a'));
    for c in [p, q, r] {
        b.join(c);
    }
    let whole = b.build(8);
    b.verdict(whole, Verdict::Red);
    assert_eq!(b.next(8).unwrap(), [p, q]);
    let part = Plan {
        changes: vec![b.member(p)],
        of: Some(whole),
    };
    assert_eq!(b.record(b.candidate_of(&part)), Err(Invalid::OutOfTurn));

    // The older half's changes all go: the newer half is next.
    b.push(p, 'b');
    b.ok(Body::Leave {
        change: q,
        why: Left::Unready,
    });
    assert_eq!(b.next(8).unwrap(), [r]);
    let newer = b.build(8);
    assert_eq!(b.ledger.candidate(newer).unwrap().of, Some(whole));
}

#[test]
fn a_dropped_candidate_waits_again_as_it_was() {
    let mut b = Book::default();
    let [p, q, r]: [ChangeId; 3] = std::array::from_fn(|i| b.change(i as u64 + 1, 'a'));
    for c in [p, q, r] {
        b.join(c);
    }
    let first = b.build(2);
    b.verdict(first, Verdict::Dropped);
    assert_eq!(b.next(8).unwrap(), [p, q, r]);

    let whole = b.build(8);
    b.verdict(whole, Verdict::Red);
    let older = b.build(8);
    b.verdict(older, Verdict::Dropped);
    // A dropped half is still a half, and goes again first.
    let late = b.change(4, 'b');
    b.join(late);
    assert_eq!(b.next(8).unwrap(), [p, q]);
    assert_eq!(b.ledger.next(&repo(), 8).unwrap().of, Some(whole));
}

#[test]
fn candidate_evidence_recorded_before_attestations_reads_as_unattested_and_writes_the_same() {
    let id = CandidateId::new().unwrap();
    let before = format!(
        r#"{{"op":"candidate_evidence","candidate":"{id}","kind":"ci","check":"check / check","result":"pass","source":"https://github.com/FieldmouseWorks/remi/actions/runs/1/job/2"}}"#
    );
    let body: Body = serde_json::from_str(&before).unwrap();
    assert!(matches!(
        &body,
        Body::CandidateEvidence { attest: None, .. }
    ));
    assert_eq!(serde_json::to_string(&body).unwrap(), before);
}

#[test]
fn evidence_on_a_candidate_is_recorded_once() {
    let mut b = Book::default();
    let a = b.change(1, 'a');
    b.join(a);
    let id = b.build(8);
    let evidence = Body::CandidateEvidence {
        candidate: id,
        kind: CheckKind::Ci,
        check: "flake".into(),
        result: Outcome::Pass,
        source: "https://github.com/FieldmouseWorks/remi/actions/runs/1".into(),
        attest: None,
    };
    b.ok(evidence.clone());
    assert_eq!(b.record(evidence.clone()), Err(Invalid::Duplicate));
    // The same result with its attestation, once checked, is more than
    // was known.
    let mut attested = evidence;
    if let Body::CandidateEvidence { attest, .. } = &mut attested {
        *attest = Some(Attest::GithubToken {
            statement: "conary commit v1\n".into(),
            token: "a.b.c".into(),
        });
    }
    b.ok(attested.clone());
    assert_eq!(b.record(attested), Err(Invalid::Duplicate));
    let unknown = CandidateId::new().unwrap();
    assert_eq!(
        b.record(Body::Verdict {
            candidate: unknown,
            verdict: Verdict::Green
        }),
        Err(Invalid::UnknownCandidate(unknown))
    );
    assert_eq!(b.ledger.candidate(id).unwrap().evidence.len(), 2);
}

#[test]
fn the_trains_ops_read_back_as_written() {
    let a = ChangeId::new().unwrap();
    let revision = RevisionId::new(&commit('0'), &commit('a'));
    let candidate = CandidateId::new().unwrap();
    let ops = [
        Body::Join {
            change: a,
            revision,
            alone: false,
        },
        Body::Leave {
            change: a,
            why: Left::Conflict,
        },
        Body::Candidate {
            candidate,
            repo: repo(),
            trunk: "main".into(),
            base: commit('0'),
            changes: vec![Member {
                change: a,
                revision,
            }],
            commit: commit('c'),
            tree: tree('7'),
            of: None,
        },
        Body::CandidateEvidence {
            candidate,
            kind: CheckKind::Ci,
            check: "check / check".into(),
            result: Outcome::Pass,
            source: "https://github.com/FieldmouseWorks/remi/actions/runs/1/job/2".into(),
            attest: Some(Attest::GithubToken {
                statement: "conary commit v1\n".into(),
                token: "a.b.c".into(),
            }),
        },
        Body::Verdict {
            candidate,
            verdict: Verdict::Dropped,
        },
        Body::Finish {
            candidate,
            tree: tree('7'),
        },
    ];
    for op in ops {
        let json = serde_json::to_string(&op).unwrap();
        assert_eq!(serde_json::from_str::<Body>(&json).unwrap(), op, "{json}");
    }
    let json = serde_json::to_value(Body::Leave {
        change: a,
        why: Left::Refused,
    })
    .unwrap();
    assert_eq!(json["op"], "leave");
    assert_eq!(json["why"], "refused");
    // A join that isn't alone is written as joins were before there was
    // `alone`, and one written then reads as not alone.
    let join = Body::Join {
        change: a,
        revision,
        alone: false,
    };
    let json = serde_json::to_value(&join).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"op": "join", "change": a, "revision": revision})
    );
    assert_eq!(serde_json::from_value::<Body>(json).unwrap(), join);
    let alone = serde_json::to_value(Body::Join {
        change: a,
        revision,
        alone: true,
    })
    .unwrap();
    assert_eq!(alone["alone"], true);
}

#[test]
fn a_change_red_alone_joins_again_only_at_a_new_head() {
    let mut b = Book::default();
    let [p, q]: [ChangeId; 2] = std::array::from_fn(|i| b.change(i as u64 + 1, 'a'));
    b.join(p);
    let alone = b.build(8);
    b.verdict(alone, Verdict::Red);
    let revision = b.current(p);
    assert_eq!(
        b.record(Body::Join {
            change: p,
            revision,
            alone: false
        }),
        Err(Invalid::Held(Held::Red))
    );
    // Trunk moving doesn't make its head any less red.
    b.join(q);
    let next = b.build(8);
    b.verdict(next, Verdict::Green);
    b.land(q, 'd').unwrap();
    assert_eq!(
        b.record(Body::Join {
            change: p,
            revision,
            alone: false
        }),
        Err(Invalid::Held(Held::Red))
    );
    b.push(p, 'b');
    b.join(p);
    assert_eq!(b.joined(), [p]);
}

#[test]
fn a_change_that_wont_merge_or_changes_nothing_joins_again_once_what_it_was_after_changes() {
    // Built after p, q won't merge, or merges to no effect; r, behind it, has
    // nothing to do with it. Both hold q as long as each other.
    for (why, as_held) in [(Left::Conflict, Held::Conflict), (Left::Empty, Held::Empty)] {
        let setup = || {
            let mut b = Book::default();
            let [p, q, r]: [ChangeId; 3] = std::array::from_fn(|i| b.change(i as u64 + 1, 'a'));
            for c in [p, q, r] {
                b.join(c);
            }
            b.ok(Body::Leave { change: q, why });
            let revision = b.current(q);
            let held = Body::Join {
                change: q,
                revision,
                alone: false,
            };
            assert_eq!(b.record(held.clone()), Err(Invalid::Held(as_held)));
            (b, [p, q, r], held)
        };

        let (mut b, [_, q, r], held) = setup();
        b.ok(Body::Leave {
            change: r,
            why: Left::Unready,
        });
        assert_eq!(b.record(held), Err(Invalid::Held(as_held)));
        b.push(q, 'b');
        b.join(q);

        // p leaving: q goes on trunk as it is.
        let (mut b, [p, q, _], held) = setup();
        b.ok(Body::Leave {
            change: p,
            why: Left::Unready,
        });
        b.ok(held);
        assert!(b.joined().contains(&q));

        // p landing: trunk moved.
        let (mut b, [p, q, _], held) = setup();
        let id = b.build(1);
        assert_eq!(b.changes(id), [p]);
        b.verdict(id, Verdict::Green);
        b.land(p, 'd').unwrap();
        b.ok(held);
        assert!(b.joined().contains(&q));

        // Something else reaching trunk moves it too.
        let (mut b, [_, q, _], held) = setup();
        b.ok(Body::Outside {
            repo: repo(),
            before: None,
            after: commit('e'),
        });
        b.ok(held);
        assert!(b.joined().contains(&q));

        // p leaving and joining again at once, before q is seen to: q isn't
        // after the same p any more, so it's let go for good.
        let (mut b, [p, q, _], held) = setup();
        b.ok(Body::Leave {
            change: p,
            why: Left::Unready,
        });
        b.join(p);
        b.ok(held);
        assert!(b.joined().contains(&q));
    }
}

#[test]
fn a_conflict_left_while_a_candidate_is_in_flight_is_after_the_changes_ahead_of_it_there() {
    let mut b = Book::default();
    let [p, q]: [ChangeId; 2] = std::array::from_fn(|i| b.change(i as u64 + 1, 'a'));
    b.join(p);
    b.join(q);
    let id = b.build(8);
    assert_eq!(b.changes(id), [p, q]);
    b.ok(Body::Leave {
        change: q,
        why: Left::Conflict,
    });
    let revision = b.current(q);
    let held = Body::Join {
        change: q,
        revision,
        alone: false,
    };
    assert_eq!(b.record(held.clone()), Err(Invalid::Held(Held::Conflict)));
    // p was ahead of it there: p leaving lets it go.
    b.ok(Body::Leave {
        change: p,
        why: Left::Unready,
    });
    b.ok(held);
}

#[test]
fn a_refused_change_joins_again_once_trunk_moves() {
    let mut b = Book::default();
    let changes: Vec<ChangeId> = (1..=3).map(|pr| b.change(pr, 'a')).collect();
    for c in &changes {
        b.join(*c);
    }
    let id = b.build(8);
    b.verdict(id, Verdict::Green);
    b.land(changes[0], 'd').unwrap();
    b.ok(Body::Leave {
        change: changes[1],
        why: Left::Refused,
    });
    let revision = b.current(changes[1]);
    let again = Body::Join {
        change: changes[1],
        revision,
        alone: false,
    };
    assert_eq!(b.record(again.clone()), Err(Invalid::Held(Held::Refused)));
    let next = b.build(8);
    b.verdict(next, Verdict::Green);
    b.land(changes[2], 'e').unwrap();
    b.ok(again);
    assert_eq!(b.joined(), [changes[1]]);
}

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

#[test]
fn only_a_green_candidate_alone_can_be_dropped() {
    let mut b = Book::default();
    let a = b.change(1, 'a');
    b.join(a);
    let id = b.build(8);
    b.verdict(id, Verdict::Green);
    let dropped = Body::Verdict {
        candidate: id,
        verdict: Verdict::Dropped,
    };
    assert_eq!(b.record(dropped), Err(Invalid::NotGating));
    b.land(a, 'd').unwrap();

    let r = b.change(2, 'b');
    b.join_alone(r);
    let id = b.build(8);
    b.verdict(id, Verdict::Green);
    let red = Body::Verdict {
        candidate: id,
        verdict: Verdict::Red,
    };
    assert_eq!(b.record(red), Err(Invalid::NotGating));
    // Trunk moved before it landed: it waits again, alone, to be built on
    // trunk as it is now.
    b.verdict(id, Verdict::Dropped);
    assert_eq!(b.ledger.candidate(id).unwrap().state, State::Dropped);
    assert_eq!(b.ledger.next(&repo(), 8).unwrap().changes, [b.member(r)]);
    let train = b.ledger.train(&repo()).unwrap();
    assert!(train.alone(r));

    // Out of the train, it's alone no more.
    b.ok(Body::Leave {
        change: r,
        why: Left::Unready,
    });
    assert!(!b.ledger.train(&repo()).unwrap().alone(r));
}

mod land;
