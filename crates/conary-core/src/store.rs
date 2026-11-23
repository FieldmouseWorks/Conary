//! The ledger open for writing: the op log and the ledger it replays to,
//! kept in step. Every write is checked against the ledger before it's
//! appended, so the log only ever holds ops that apply.

use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

use ed25519_dalek::SigningKey;

use crate::id::{ChangeId, CommitId, OpId};
use crate::ledger::{Invalid, Ledger};
use crate::log::{self, Anchor, Lock, LogError, OpLog};
use crate::op::{Body, Repo};

#[derive(Debug)]
pub enum StoreError {
    Log(LogError),
    /// The op can't be applied; nothing was written.
    Invalid(Invalid),
    /// The log verifies but holds an op that doesn't apply.
    Replay(OpId, Invalid),
    /// Another writer recorded about what the op is about since it was read
    /// ([`Store::reading`]), so the op may say it as it was before that;
    /// nothing was written.
    Overtaken,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Log(e) => e.fmt(f),
            Self::Invalid(e) => e.fmt(f),
            Self::Replay(id, e) => write!(f, "op {id} in the log doesn't apply: {e}"),
            Self::Overtaken => {
                f.write_str("another writer recorded about it since it was read: read it again")
            }
        }
    }
}

impl std::error::Error for StoreError {}

impl From<LogError> for StoreError {
    fn from(e: LogError) -> Self {
        Self::Log(e)
    }
}

/// The ledger open for writing. It holds the writer lock only while it
/// writes: each op, or each batch under one [`Store::writer`], takes it, and
/// first reads what other writers appended since, so every op is admitted
/// against the ledger as the log holds it then. That alone would let a
/// writer record something as it was before what another recorded while it
/// read, so an op written without a hold ([`Store::hold`]) is refused once
/// another writer recorded about the same thing since [`Store::reading`].
pub struct Store {
    log: OpLog,
    ledger: Ledger,
    /// An op another writer appended that doesn't apply: the log and the
    /// ledger no longer agree, so nothing more is written.
    broken: Option<(OpId, Invalid)>,
    /// What other writers recorded about since [`Store::reading`].
    overtaken: HashSet<About>,
}

/// What an op says something of, as far as another's can make it out of
/// date. A train's own ops are none's: they're written only under a hold.
#[derive(Clone, PartialEq, Eq, Hash)]
enum About {
    Change(ChangeId),
    /// A commit trunk moved to, by a landing or outside one.
    Trunk(Repo, CommitId),
}

impl About {
    /// All `body` is about. A landing is about its change and, in that
    /// change's repository as `ledger` holds it, the commit it moved trunk
    /// to: the same commit an outside move there is about.
    fn of(ledger: &Ledger, body: &Body) -> impl Iterator<Item = Self> {
        let moved = match body {
            Body::Land { change, after, .. } => ledger.change(*change).map(|c| (&c.repo, after)),
            Body::Outside { repo, after, .. } => Some((repo, after)),
            _ => None,
        };
        let trunk = moved.map(|(repo, after)| Self::Trunk(repo.clone(), after.clone()));
        body.change().map(Self::Change).into_iter().chain(trunk)
    }
}

impl Store {
    /// Opens the ledger in `lock`'s directory for writing, refusing a log
    /// that doesn't hold `anchor`, the newest one its caller holds (and read
    /// under that lock, so no other writer moved it since). The lock is let
    /// go once it's open.
    pub fn open(lock: Lock, key: SigningKey, anchor: Option<Anchor>) -> Result<Self, StoreError> {
        let (log, ops) = OpLog::open(lock, key, anchor)?;
        let ledger = Ledger::replay(&ops).map_err(|(id, e)| StoreError::Replay(id, e))?;
        Ok(Self {
            log,
            ledger,
            broken: None,
            overtaken: HashSet::new(),
        })
    }

    /// The ledger as of this store's last write, or its opening: another
    /// writer's ops are read at the next write.
    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    /// The last op recorded, to anchor the log at.
    pub fn head(&self) -> Option<Anchor> {
        self.log.head()
    }

    /// The ledger's directory.
    pub fn dir(&self) -> &std::path::Path {
        self.log.dir()
    }

    /// Takes the writer lock, waiting for any other writer, and applies what
    /// other writers appended since: a batch of writes, until it's dropped.
    pub fn writer(&mut self) -> Result<Writer<'_>, StoreError> {
        self.whole()?;
        let (log, ops) = self.log.writer()?;
        Self::catch_up(
            &mut self.ledger,
            &mut self.broken,
            &mut self.overtaken,
            &ops,
        )?;
        Ok(Writer {
            log,
            ledger: &mut self.ledger,
            overtaken: &self.overtaken,
        })
    }

    /// The ledger as the log holds it now: what other writers appended since
    /// this store's last write applied, under the lock for only as long as
    /// that takes.
    pub fn refresh(&mut self) -> Result<&Ledger, StoreError> {
        drop(self.writer()?);
        Ok(&self.ledger)
    }

    /// Starts a read that writes are to be decided from, and returns the
    /// ledger as it stands then. Call it before reading GitHub: what another
    /// writer records from here on refuses an op of this store's about the
    /// same change, or the same trunk commit's move ([`StoreError::Overtaken`]).
    pub fn reading(&mut self) -> Result<&Ledger, StoreError> {
        self.refresh()?;
        self.overtaken.clear();
        Ok(&self.ledger)
    }

    /// Whether another writer recorded about `change` since
    /// [`Store::reading`], as far as this store has caught up.
    pub fn overtaken(&self, change: ChangeId) -> bool {
        self.overtaken.contains(&About::Change(change))
    }

    /// Takes the writer lock, applies what other writers appended, and
    /// holds it, every write after included, until the store is dropped:
    /// for a writer whose reads, of the ledger and GitHub, must still hold
    /// when it writes, such as a `cn` command.
    pub fn hold(&mut self) -> Result<(), StoreError> {
        self.whole()?;
        let ops = self.log.hold()?;
        let caught = Self::catch_up(
            &mut self.ledger,
            &mut self.broken,
            &mut self.overtaken,
            &ops,
        );
        if caught.is_err() {
            self.log.release();
        }
        caught
    }

    /// [`Store::hold`] through `then` only, such as a train step. Within
    /// another's hold, `then` just runs.
    pub fn holding<T>(&mut self, then: impl FnOnce(&mut Self) -> T) -> Result<T, StoreError> {
        let outer = self.log.holds();
        self.hold()?;
        let done = then(self);
        if !outer {
            self.log.release();
        }
        Ok(done)
    }

    /// Refuses to write once the log and the ledger disagree.
    fn whole(&self) -> Result<(), StoreError> {
        match &self.broken {
            Some((id, e)) => Err(StoreError::Replay(*id, e.clone())),
            None => Ok(()),
        }
    }

    /// Applies `ops`, other writers' since the last write.
    fn catch_up(
        ledger: &mut Ledger,
        broken: &mut Option<(OpId, Invalid)>,
        overtaken: &mut HashSet<About>,
        ops: &[crate::op::Op],
    ) -> Result<(), StoreError> {
        for op in ops {
            if let Err(e) = ledger.check(&op.op.body) {
                *broken = Some((op.id, e.clone()));
                return Err(StoreError::Replay(op.id, e));
            }
            ledger.apply(op);
            overtaken.extend(About::of(ledger, &op.op.body));
        }
        Ok(())
    }

    /// Records one op, now.
    pub fn record(&mut self, body: Body) -> Result<OpId, StoreError> {
        self.record_at(body, now())
    }

    pub fn record_at(&mut self, body: Body, at: i64) -> Result<OpId, StoreError> {
        self.writer()?.record_at(body, at)
    }
}

/// The ledger under its writer lock: what [`Store::writer`] returns. The
/// lock goes when it's dropped.
pub struct Writer<'a> {
    log: log::Writer<'a>,
    ledger: &'a mut Ledger,
    overtaken: &'a HashSet<About>,
}

impl Writer<'_> {
    pub fn ledger(&self) -> &Ledger {
        self.ledger
    }

    /// The last op recorded, to anchor the log at.
    pub fn head(&self) -> Option<Anchor> {
        self.log.head()
    }

    /// The writer lock this batch holds.
    pub fn lock(&self) -> &Lock {
        self.log.lock()
    }

    /// Whether the log holds `anchor`.
    pub fn check(&self, anchor: Anchor) -> Result<(), LogError> {
        self.log.check(anchor)
    }

    /// Records one op, now.
    pub fn record(&mut self, body: Body) -> Result<OpId, StoreError> {
        self.record_at(body, now())
    }

    pub fn record_at(&mut self, body: Body, at: i64) -> Result<OpId, StoreError> {
        self.ledger.admit(&body).map_err(StoreError::Invalid)?;
        // Checked after admission, so an op that says only what another
        // writer already said is the duplicate it always was. Under a hold
        // what's written was read under it.
        if !self.log.held() && About::of(self.ledger, &body).any(|a| self.overtaken.contains(&a)) {
            return Err(StoreError::Overtaken);
        }
        let op = self.log.append(body, at)?;
        self.ledger.apply(&op);
        Ok(op.id)
    }
}

/// Milliseconds since the Unix epoch.
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{ChangeId, CommitId, RevisionId};
    use crate::ledger::Status;
    use crate::log;
    use crate::op::{Actor, ActorKind, Attest, Authority, CheckKind, Outcome, Repo};
    use crate::policy::Class;

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[3; 32])
    }

    fn repo() -> Repo {
        Repo {
            owner: "FieldmouseWorks".into(),
            name: "burrow".into(),
        }
    }

    fn commit(c: char) -> CommitId {
        c.to_string().repeat(40).parse().unwrap()
    }

    fn observe(store: &mut Store, pr: u64) -> ChangeId {
        let change = ChangeId::new().unwrap();
        store
            .record(Body::Observe {
                change,
                repo: repo(),
                pr,
                author: Actor {
                    kind: ActorKind::Agent,
                    login: "TusanHomichi".into(),
                    session: Some("https://claude.ai/code/session_x".into()),
                },
            })
            .unwrap();
        change
    }

    /// What a writer read before another recorded about it is refused, not
    /// appended after it as the newer word; what both say is only said once.
    #[test]
    fn an_op_decided_before_anothers_about_the_same_thing_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let open = || Store::open(Lock::take(dir.path()).unwrap(), key(), None).unwrap();
        let mut a = open();
        let (change, other) = (observe(&mut a, 1), observe(&mut a, 2));
        let revision = |change, head| Body::Revision {
            change,
            base: commit('a'),
            head: commit(head),
        };
        let outside = || Body::Outside {
            repo: repo(),
            before: None,
            after: commit('f'),
        };
        let mut b = open();
        a.reading().unwrap();
        b.record(revision(change, 'c')).unwrap();
        b.record(outside()).unwrap();

        assert!(matches!(
            a.record(revision(change, 'b')),
            Err(StoreError::Overtaken)
        ));
        assert!(matches!(a.record(outside()), Err(StoreError::Overtaken)));
        assert!(matches!(
            a.record(revision(change, 'c')),
            Err(StoreError::Invalid(Invalid::Duplicate))
        ));
        assert!(a.overtaken(change));
        // What the other didn't touch is recorded.
        a.record(revision(other, 'b')).unwrap();
        // Catching up alone doesn't make an earlier read current.
        a.refresh().unwrap();
        assert!(matches!(
            a.record(revision(change, 'b')),
            Err(StoreError::Overtaken)
        ));
        // Reading again does, and so does reading under a hold.
        a.reading().unwrap();
        a.record(revision(change, 'd')).unwrap();
        b.holding(|b| b.record(revision(change, 'e')))
            .unwrap()
            .unwrap();
    }

    /// One commit trunk moved to gets one verdict: whichever of a landing
    /// and an outside move another writer recorded there overtakes the other.
    #[test]
    fn a_land_and_an_outside_at_one_trunk_commit_overtake_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let open = || Store::open(Lock::take(dir.path()).unwrap(), key(), None).unwrap();
        let mut a = open();
        let mut changes = Vec::new();
        for pr in [1, 2] {
            let change = observe(&mut a, pr);
            a.record(Body::Revision {
                change,
                base: commit('a'),
                head: commit('b'),
            })
            .unwrap();
            changes.push(change);
        }
        let land = |change, after| Body::Land {
            change,
            revision: RevisionId::new(&commit('a'), &commit('b')),
            before: commit('a'),
            after: commit(after),
            by: Authority::Status {
                login: "steward".into(),
            },
        };
        // GitHub ignores case in a repository's name.
        let outside = |after| Body::Outside {
            repo: "fieldmouseworks/Burrow".parse().unwrap(),
            before: None,
            after: commit(after),
        };
        let mut b = open();
        a.reading().unwrap();
        b.record(land(changes[0], 'c')).unwrap();
        assert!(matches!(a.record(outside('c')), Err(StoreError::Overtaken)));
        assert!(a.overtaken(changes[0]));
        // Trunk moved where no landing is stays a violation.
        a.record(outside('d')).unwrap();

        a.reading().unwrap();
        b.record(outside('e')).unwrap();
        assert!(matches!(
            a.record(land(changes[1], 'e')),
            Err(StoreError::Overtaken)
        ));
        // It overtakes a landing there, not the change, or one elsewhere.
        assert!(!a.overtaken(changes[1]));
        a.record(land(changes[1], 'f')).unwrap();
    }

    /// Each degraded path is written as the op log spells it, and a replay
    /// gives back the same degradations, cleared as they were.
    #[test]
    fn degradations_are_written_and_replayed_as_spelled() {
        use crate::op::Degraded;
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(Lock::take(dir.path()).unwrap(), key(), None).unwrap();
        let change = observe(&mut store, 9);
        store
            .record(Body::Revision {
                change,
                base: commit('a'),
                head: commit('b'),
            })
            .unwrap();
        let revision = RevisionId::new(&commit('a'), &commit('b'));
        let paths = [
            (Degraded::Record, r#"{"path":"record"}"#),
            (
                Degraded::Answer {
                    job: "3-1".into(),
                    kind: crate::plugin::Kind::Proof,
                },
                r#"{"path":"answer","job":"3-1","kind":"proof"}"#,
            ),
            (
                Degraded::Lookup {
                    check: "gate".into(),
                },
                r#"{"path":"lookup","check":"gate"}"#,
            ),
            (
                Degraded::Read {
                    read: crate::op::Reading::MergeTree,
                },
                r#"{"path":"read","read":"merge_tree"}"#,
            ),
            (
                Degraded::Mark {
                    mark: crate::op::Mark::ReleaseAge,
                },
                r#"{"path":"mark","mark":"release_age"}"#,
            ),
        ];
        for (what, spelled) in &paths {
            assert_eq!(serde_json::to_string(what).unwrap(), *spelled);
            store
                .record(Body::Degraded {
                    change,
                    revision,
                    what: what.clone(),
                    why: "it didn't read".into(),
                })
                .unwrap();
        }
        store
            .record(Body::Restored {
                change,
                revision,
                what: Degraded::Record,
            })
            .unwrap();
        let written = store.ledger().change(change).unwrap().degradations.clone();
        drop(store);
        let ops = log::read(dir.path(), &key().verifying_key(), None).unwrap();
        let replayed = Ledger::replay(&ops).unwrap();
        let c = replayed.change(change).unwrap();
        assert_eq!(c.degradations, written);
        assert_eq!(c.degradations.len(), paths.len());
        assert_eq!(c.degraded(revision).count(), paths.len() - 1);
    }

    #[test]
    fn a_change_is_seen_revised_checked_and_landed() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(Lock::take(dir.path()).unwrap(), key(), None).unwrap();
        let change = observe(&mut store, 9);
        store
            .record(Body::Revision {
                change,
                base: commit('a'),
                head: commit('b'),
            })
            .unwrap();
        let revision = RevisionId::new(&commit('a'), &commit('b'));
        store
            .record(Body::Evidence {
                change,
                revision,
                kind: CheckKind::Ci,
                check: "check / check".into(),
                result: Outcome::Pass,
                source: "https://github.com/x/y/actions/runs/1".into(),
                attest: None,
            })
            .unwrap();
        store
            .record(Body::Land {
                change,
                revision,
                before: commit('a'),
                after: commit('c'),
                by: Authority::Status {
                    login: "steward".into(),
                },
            })
            .unwrap();
        drop(store);

        let ops = log::read(dir.path(), &key().verifying_key(), None).unwrap();
        let ledger = Ledger::replay(&ops).unwrap();
        let landed = ledger.landed_as(&repo(), &commit('c')).unwrap();
        assert_eq!(landed.id, change);
        assert_eq!(landed.pr, 9);
        assert_eq!(landed.status(), Status::Landed);
        assert_eq!(landed.evidence.len(), 1);
        assert_eq!(ledger.by_pr(&repo(), 9).unwrap().id, change);
        // GitHub ignores case in a repository's name, and so does the ledger.
        let spelled = Repo {
            owner: "fieldmouseworks".into(),
            name: "Burrow".into(),
        };
        assert_eq!(ledger.landed_as(&spelled, &commit('c')).unwrap().id, change);
    }

    /// A result is written as results were before attestations, and reads
    /// as unattested; the same result attested is recorded beside it, once,
    /// and kept through a replay.
    #[test]
    fn a_result_keeps_its_attestation_and_is_written_as_before_without_one() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(Lock::take(dir.path()).unwrap(), key(), None).unwrap();
        let change = observe(&mut store, 4);
        store
            .record(Body::Revision {
                change,
                base: commit('a'),
                head: commit('b'),
            })
            .unwrap();
        let revision = RevisionId::new(&commit('a'), &commit('b'));
        let evidence = |attest| Body::Evidence {
            change,
            revision,
            kind: CheckKind::Negative,
            check: "check / negative".into(),
            result: Outcome::Fail,
            source: "https://github.com/x/y/actions/runs/1/job/2".into(),
            attest,
        };
        let json = serde_json::to_value(evidence(None)).unwrap();
        assert!(json.get("attest").is_none(), "{json}");
        assert_eq!(
            serde_json::from_value::<Body>(json).unwrap(),
            evidence(None)
        );
        let attest = Attest::GithubToken {
            statement: "conary negative v1\n".into(),
            token: "t".into(),
        };
        store.record(evidence(None)).unwrap();
        store.record(evidence(Some(attest.clone()))).unwrap();
        assert!(matches!(
            store.record(evidence(Some(attest.clone()))),
            Err(StoreError::Invalid(Invalid::Duplicate))
        ));
        drop(store);
        let ops = log::read(dir.path(), &key().verifying_key(), None).unwrap();
        let ledger = Ledger::replay(&ops).unwrap();
        let kept: Vec<_> = ledger
            .change(change)
            .unwrap()
            .evidence
            .iter()
            .map(|e| e.attest.clone())
            .collect();
        assert_eq!(kept, [None, Some(attest)]);
        // A negative run that found no test to run says so in its own word.
        assert_eq!(serde_json::to_value(Outcome::None).unwrap(), "none");
    }

    /// A revision's class is recorded once for a policy's word on it, and a
    /// later word (another policy, say) supersedes it; a revision the
    /// change doesn't have is refused.
    #[test]
    fn a_revisions_class_is_recorded_once_and_the_latest_counts() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(Lock::take(dir.path()).unwrap(), key(), None).unwrap();
        let change = observe(&mut store, 5);
        store
            .record(Body::Revision {
                change,
                base: commit('a'),
                head: commit('b'),
            })
            .unwrap();
        let revision = RevisionId::new(&commit('a'), &commit('b'));
        let classify = |revision, class, policy: &str| Body::Classify {
            change,
            revision,
            class,
            because: (class == Class::R2).then(|| "CODEOWNERS".into()),
            docs: false,
            policy: policy.into(),
            raised: Vec::new(),
        };
        let labelled = |class, raised: &[&str]| Body::Classify {
            change,
            revision,
            class,
            because: None,
            docs: false,
            policy: "p1".into(),
            raised: raised.iter().map(|l| (*l).to_owned()).collect(),
        };
        store.record(classify(revision, Class::R0, "p1")).unwrap();
        assert!(matches!(
            store.record(classify(revision, Class::R0, "p1")),
            Err(StoreError::Invalid(Invalid::Duplicate))
        ));
        let elsewhere = RevisionId::new(&commit('a'), &commit('c'));
        assert!(matches!(
            store.record(classify(elsewhere, Class::R0, "p1")),
            Err(StoreError::Invalid(Invalid::UnknownRevision(_)))
        ));
        // A raise label says what it raised: `r2` only on R2, the names
        // sorted, and nothing that isn't a raise label.
        for (class, raised) in [
            (Class::R0, vec!["r2"]),
            (Class::R2, vec!["review", "r2"]),
            (Class::R0, vec!["bug"]),
            (Class::R0, vec!["Review"]),
        ] {
            let body = labelled(class, &raised);
            assert!(
                matches!(
                    store.record(body),
                    Err(StoreError::Invalid(Invalid::NotRaised))
                ),
                "{class:?}"
            );
        }
        store.record(labelled(Class::R0, &["review"])).unwrap();
        // A raise once recorded stays: a class without it is refused.
        assert!(matches!(
            store.record(classify(revision, Class::R2, "p2")),
            Err(StoreError::Invalid(Invalid::NotRaised))
        ));
        store
            .record(Body::Classify {
                change,
                revision,
                class: Class::R2,
                because: Some("CODEOWNERS".into()),
                docs: false,
                policy: "p2".into(),
                raised: vec!["review".into()],
            })
            .unwrap();
        // A join names what raised its change, as the steward settled it.
        assert!(matches!(
            store.record(Body::Join {
                change,
                revision,
                alone: true,
                raised: Vec::new(),
            }),
            Err(StoreError::Invalid(Invalid::JoinRaised))
        ));
        let review = || vec!["review".to_owned()];
        // R2 rides alone: a join may raise its class, never lower it.
        assert!(matches!(
            store.record(Body::Join {
                change,
                revision,
                alone: false,
                raised: review(),
            }),
            Err(StoreError::Invalid(Invalid::BelowClass(r))) if r == revision
        ));
        store
            .record(Body::Join {
                change,
                revision,
                alone: true,
                raised: review(),
            })
            .unwrap();
        drop(store);
        let ops = log::read(dir.path(), &key().verifying_key(), None).unwrap();
        let ledger = Ledger::replay(&ops).unwrap();
        let c = ledger.change(change).unwrap();
        let now = c.class_of(revision).unwrap();
        assert_eq!((now.class, now.policy.as_str()), (Class::R2, "p2"));
        assert_eq!(now.because.as_deref(), Some("CODEOWNERS"));
        assert_eq!(c.classes.len(), 3);
        assert_eq!(c.classes[1].raised, ["review"]);
        assert!(c.class_of(elsewhere).is_none());
        assert!(ledger.train(&c.repo).unwrap().alone(change));
    }

    #[test]
    fn ops_that_dont_apply_are_refused_and_never_written() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(Lock::take(dir.path()).unwrap(), key(), None).unwrap();
        let change = observe(&mut store, 1);
        let unknown = RevisionId::new(&commit('a'), &commit('d'));
        let refused = [
            Body::Revision {
                change: ChangeId::new().unwrap(),
                base: commit('a'),
                head: commit('b'),
            },
            Body::Land {
                change,
                revision: unknown,
                before: commit('a'),
                after: commit('c'),
                by: Authority::Status {
                    login: "steward".into(),
                },
            },
            Body::Reopen { change },
            Body::Observe {
                change: ChangeId::new().unwrap(),
                repo: repo(),
                pr: 1,
                author: store.ledger().change(change).unwrap().author.clone(),
            },
        ];
        for body in refused {
            assert!(matches!(store.record(body), Err(StoreError::Invalid(_))));
        }
        let revision = Body::Revision {
            change,
            base: commit('a'),
            head: commit('b'),
        };
        store.record(revision.clone()).unwrap();
        assert!(matches!(
            store.record(revision),
            Err(StoreError::Invalid(Invalid::Duplicate))
        ));
        store.record(Body::Close { change }).unwrap();
        let revision = RevisionId::new(&commit('a'), &commit('b'));
        let land = Body::Land {
            change,
            revision,
            before: commit('a'),
            after: commit('c'),
            by: Authority::Status {
                login: "steward".into(),
            },
        };
        assert!(matches!(
            store.record(land),
            Err(StoreError::Invalid(Invalid::Closed))
        ));
        drop(store);
        assert_eq!(
            log::read(dir.path(), &key().verifying_key(), None)
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn changes_are_listed_in_the_order_they_were_seen() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(Lock::take(dir.path()).unwrap(), key(), None).unwrap();
        let seen: Vec<ChangeId> = (1..=20).map(|pr| observe(&mut store, pr)).collect();
        let listed: Vec<ChangeId> = store.ledger().changes().map(|c| c.id).collect();
        assert_eq!(listed, seen);
    }

    #[test]
    fn historical_signed_undos_replay_without_rewriting_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(Lock::take(dir.path()).unwrap(), key(), None).unwrap();
        let x = observe(&mut store, 1);
        store
            .record(Body::Revision {
                change: x,
                base: commit('a'),
                head: commit('b'),
            })
            .unwrap();
        store
            .record(Body::Land {
                change: x,
                revision: RevisionId::new(&commit('a'), &commit('b')),
                before: commit('a'),
                after: commit('c'),
                by: Authority::Status {
                    login: "steward".into(),
                },
            })
            .unwrap();
        let u = observe(&mut store, 2);
        let anchor = store.head();
        drop(store);
        // Simulate the old writer: a signed undo with exactly the historical
        // fields, appended below today's admission rules.
        let json = serde_json::json!({
            "op": "undo", "change": u, "reverts": [x], "asked": {"by": "change"},
        });
        let body: Body = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(serde_json::to_value(&body).unwrap(), json);
        let (mut log, _) = OpLog::open(Lock::take(dir.path()).unwrap(), key(), anchor).unwrap();
        log.writer().unwrap().0.append(body, now()).unwrap();
        let anchor = log.head();
        drop(log);
        let path = dir.path().join("ops.jsonl");
        let before = std::fs::read(&path).unwrap();
        let store = Store::open(Lock::take(dir.path()).unwrap(), key(), anchor).unwrap();
        assert_eq!(store.ledger().undo_of(x), Some(u));
        assert_eq!(
            store
                .ledger()
                .change(u)
                .unwrap()
                .undo
                .as_ref()
                .unwrap()
                .reverts,
            [x]
        );
        assert_eq!(std::fs::read(path).unwrap(), before);
    }

    #[test]
    fn two_changes_cant_claim_one_trunk_commit() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(Lock::take(dir.path()).unwrap(), key(), None).unwrap();
        for pr in [1, 2] {
            let change = observe(&mut store, pr);
            store
                .record(Body::Revision {
                    change,
                    base: commit('a'),
                    head: commit('b'),
                })
                .unwrap();
            let land = store.record(Body::Land {
                change,
                revision: RevisionId::new(&commit('a'), &commit('b')),
                before: commit('a'),
                after: commit('c'),
                by: Authority::Status {
                    login: "steward".into(),
                },
            });
            assert_eq!(land.is_ok(), pr == 1);
        }
    }

    #[test]
    fn trunk_can_move_to_the_same_commit_more_than_once() {
        // Trunk at c, moved away, then reset to c, and reset to a landed
        // commit: each is a movement of its own.
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(Lock::take(dir.path()).unwrap(), key(), None).unwrap();
        let change = observe(&mut store, 1);
        store
            .record(Body::Revision {
                change,
                base: commit('a'),
                head: commit('b'),
            })
            .unwrap();
        store
            .record(Body::Land {
                change,
                revision: RevisionId::new(&commit('a'), &commit('b')),
                before: commit('a'),
                after: commit('d'),
                by: Authority::Status {
                    login: "steward".into(),
                },
            })
            .unwrap();
        for after in ['c', 'e', 'c', 'd'] {
            store
                .record(Body::Outside {
                    repo: repo(),
                    before: None,
                    after: commit(after),
                })
                .unwrap();
        }
        assert_eq!(store.ledger().outside().len(), 4);
    }

    #[test]
    fn a_store_whose_log_lost_its_anchored_op_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(Lock::take(dir.path()).unwrap(), key(), None).unwrap();
        observe(&mut store, 1);
        let anchor = store.head().unwrap();
        drop(store);
        Store::open(Lock::take(dir.path()).unwrap(), key(), Some(anchor)).unwrap();
        std::fs::remove_file(dir.path().join("ops.jsonl")).unwrap();
        assert!(matches!(
            Store::open(Lock::take(dir.path()).unwrap(), key(), Some(anchor)),
            Err(StoreError::Log(LogError::Unanchored(_)))
        ));
    }
}
