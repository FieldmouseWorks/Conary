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

    /// A new head on the change.
    fn push(&mut self, change: ChangeId, head: char) -> RevisionId {
        self.ok(Body::Revision {
            change,
            base: commit('0'),
            head: commit(head),
        });
        self.newest(change)
    }

    fn newest(&self, change: ChangeId) -> RevisionId {
        self.ledger
            .change(change)
            .unwrap()
            .revisions
            .last()
            .unwrap()
            .id
    }

    fn member(&self, change: ChangeId) -> Member {
        Member {
            change,
            revision: self.newest(change),
        }
    }

    fn join(&mut self, change: ChangeId) {
        let revision = self.newest(change);
        self.ok(Body::Join { change, revision });
    }

    fn candidate_of(&self, plan: &Plan) -> Body {
        Body::Candidate {
            candidate: CandidateId::new().unwrap(),
            repo: repo(),
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
        let revision = self.newest(change);
        self.record(Body::Land {
            change,
            revision,
            before: commit('0'),
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
fn a_change_joins_once_at_a_revision_and_again_at_another_at_the_back() {
    let mut b = Book::default();
    let a = b.change(1, 'a');
    let old = b.newest(a);
    let unknown = RevisionId::new(&commit('0'), &commit('f'));
    assert_eq!(
        b.record(Body::Join {
            change: a,
            revision: unknown
        }),
        Err(Invalid::UnknownRevision(unknown))
    );
    let new = b.push(a, 'b');
    b.join(a);
    assert_eq!(
        b.record(Body::Join {
            change: a,
            revision: new
        }),
        Err(Invalid::Duplicate)
    );
    let c = b.change(2, 'c');
    b.join(c);

    // Its head pushed back to an older one: a revision the ledger already
    // holds, and the PR's head now.
    b.ok(Body::Join {
        change: a,
        revision: old,
    });
    assert_eq!(b.joined(), [c, a]);
    assert_eq!(b.ledger.train(&repo()).unwrap().joined()[1].revision, old);

    // In a candidate in flight, it stops the candidate.
    let id = b.build(8);
    b.ok(Body::Join {
        change: a,
        revision: new,
    });
    assert_eq!(b.state(id), State::Broken { by: a });
    assert_eq!(b.next(8).unwrap(), [c, a]);

    let closed = b.change(3, 'd');
    b.ok(Body::Close { change: closed });
    assert_eq!(
        b.record(Body::Join {
            change: closed,
            revision: b.newest(closed)
        }),
        Err(Invalid::Closed)
    );
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
    assert_eq!(b.state(id), State::Broken { by: c });
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
    assert_eq!(b.state(id), State::Broken { by: c });
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
    assert_eq!(b.state(id), State::Broken { by: changes[1] });
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
    };
    b.ok(evidence.clone());
    assert_eq!(b.record(evidence), Err(Invalid::Duplicate));
    let unknown = CandidateId::new().unwrap();
    assert_eq!(
        b.record(Body::Verdict {
            candidate: unknown,
            verdict: Verdict::Green
        }),
        Err(Invalid::UnknownCandidate(unknown))
    );
    assert_eq!(b.ledger.candidate(id).unwrap().evidence.len(), 1);
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
        },
        Body::Leave {
            change: a,
            why: Left::Conflict,
        },
        Body::Candidate {
            candidate,
            repo: repo(),
            base: commit('0'),
            changes: vec![Member {
                change: a,
                revision,
            }],
            commit: commit('c'),
            tree: tree('7'),
            of: None,
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
}
