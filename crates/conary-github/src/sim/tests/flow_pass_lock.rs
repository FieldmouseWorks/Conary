//! A flow pass that writes to the ledger holds its lock only while it
//! writes: a read of GitHub that hangs holds up no other writer, and what
//! the other writer recorded and anchored meanwhile is kept by both.
use super::flow_pass_case::{Case, REPO};
use crate::anchor;
use crate::sim::{Call, unblocked};
use conary_core::ChangeId;
use conary_core::log;
use conary_core::op::{Actor, ActorKind, Body};
use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;

const ANCHOR: &str = "conary-test/ledger";

/// Another repository's writer: open against the anchor, record its `pr`,
/// publish.
fn other_writer(dir: &Path, hub: &crate::sim::Hub, pr: u64) -> conary_core::OpId {
    let key = log::load_signing_key(&log::key_path(dir)).unwrap();
    let mut other = anchor::open(dir, key, hub, ANCHOR).unwrap();
    let id = other
        .store
        .record(Body::Observe {
            change: ChangeId::new().unwrap(),
            repo: "conary-test/other".parse().unwrap(),
            pr,
            author: Actor {
                kind: ActorKind::Person,
                login: "someone".into(),
                session: None,
            },
        })
        .unwrap();
    other.publish(hub).unwrap();
    id
}

#[test]
fn a_pass_whose_read_hangs_holds_up_no_other_writer() {
    unblocked(120, || {
        let c = Case::new();
        let ledger = tempfile::tempdir().unwrap();
        let dir = ledger.path().to_owned();
        anchor::init(&dir, &log::key_path(&dir), &c.hub, ANCHOR).unwrap();
        let key = log::load_signing_key(&log::key_path(&dir)).unwrap();
        let mut anchored = anchor::open(&dir, key, &c.hub, ANCHOR).unwrap();
        // Two of the pass's reads of its PR hang, one before it records
        // anything and one after, while another repository's writer takes
        // the lock, records and moves the anchor.
        let wrote: Vec<Rc<Cell<Option<conary_core::OpId>>>> = vec![Rc::default(), Rc::default()];
        for (pr, (call, theirs)) in (1..).zip(
            [Call::FlowSnapshot, Call::FlowStage]
                .into_iter()
                .zip(&wrote),
        ) {
            let (theirs, at) = (Rc::clone(theirs), dir.clone());
            c.hub.race(call.repo(REPO).pr(3), move |hub| {
                theirs.set(Some(other_writer(&at, hub, pr)));
            });
        }
        let pass = crate::flow::Pass {
            config: &c.config,
            policy: &c.policy,
            root: c.dir.path(),
            launcher: &c.deferred,
            now: crate::flow::jobs::now(),
            dry_run: false,
            ledger: None,
        };
        pass.run(
            &c.hub,
            &REPO.parse().unwrap(),
            None,
            Some(&mut anchored.store),
        )
        .unwrap();
        anchored.publish(&c.hub).unwrap();

        // Both writers' ops are in the log, the pass's between the other's,
        // each chained on the one before, and the anchor names the log's
        // head.
        let public = log::load_public_key(&log::public_path(&log::key_path(&dir))).unwrap();
        let ops = log::read(&dir, &public, None).unwrap();
        let at: Vec<usize> = wrote
            .iter()
            .map(|w| {
                let theirs = w.get().expect("the other writer ran");
                ops.iter().position(|o| o.id == theirs).unwrap()
            })
            .collect();
        assert!(at[0] + 1 < at[1], "{at:?} of {}", ops.len());
        let head = log::Anchor {
            seq: ops.len(),
            op: ops.last().unwrap().id,
        };
        assert_eq!(
            anchor::verify(&dir, &public, &c.hub, ANCHOR).unwrap(),
            Some(head)
        );
        assert_eq!(anchored.store.head(), Some(head));
        let other = "conary-test/other".parse().unwrap();
        for pr in [1, 2] {
            assert!(anchored.store.ledger().by_pr(&other, pr).is_some());
        }
    });
}

/// A ledger of its own for `c`, set up and anchored, open for a pass.
fn anchored(c: &Case) -> (tempfile::TempDir, anchor::Anchored) {
    let ledger = tempfile::tempdir().unwrap();
    let dir = ledger.path();
    anchor::init(dir, &log::key_path(dir), &c.hub, ANCHOR).unwrap();
    let key = log::load_signing_key(&log::key_path(dir)).unwrap();
    let anchored = anchor::open(dir, key, &c.hub, ANCHOR).unwrap();
    (ledger, anchored)
}

/// One pass of `c`, published after; the error its PR 3 met, if any.
fn pass(c: &Case, anchored: &mut anchor::Anchored) -> Option<String> {
    let pass = crate::flow::Pass {
        config: &c.config,
        policy: &c.policy,
        root: c.dir.path(),
        launcher: &c.deferred,
        now: crate::flow::jobs::now(),
        dry_run: false,
        ledger: None,
    };
    let reports = pass
        .run(
            &c.hub,
            &REPO.parse().unwrap(),
            None,
            Some(&mut anchored.store),
        )
        .unwrap();
    anchored.publish(&c.hub).unwrap();
    reports
        .into_iter()
        .find(|r| r.pr == 3)
        .and_then(|r| r.error)
}

/// What the ledger at `dir`, replayed from its log, holds of PR 3: each op
/// about it by kind, in order, and its change as the ledger has it.
fn pr_3(dir: &Path) -> (Vec<String>, String) {
    let public = log::load_public_key(&log::public_path(&log::key_path(dir))).unwrap();
    let ledger = conary_core::reader::read(dir, &public).unwrap();
    let c = ledger.by_pr(&REPO.parse().unwrap(), 3).unwrap();
    let kinds = log::read(dir, &public, None)
        .unwrap()
        .iter()
        .map(|o| {
            let body = serde_json::to_value(&o.op.body).unwrap();
            body["op"].as_str().unwrap().to_owned()
        })
        .collect();
    let evidence: Vec<_> = c.evidence.iter().map(|e| (&e.check, &e.result)).collect();
    let classes: Vec<_> = c.classes.iter().map(|c| c.class).collect();
    let change = format!(
        "{:?} revisions {} evidence {evidence:?} classes {classes:?}",
        c.status(),
        c.revisions.len(),
    );
    (kinds, change)
}

/// Another writer, ingesting the case's PR 3 into the ledger at `dir` just
/// before the pass's next read of it: the ops it recorded, once it has.
fn race_pull_of_3(c: &Case, dir: &Path) -> Rc<Cell<Option<usize>>> {
    let wrote: Rc<Cell<Option<usize>>> = Rc::default();
    let (theirs, at) = (Rc::clone(&wrote), dir.to_owned());
    let settings = c.config.settings.clone();
    c.hub.race(Call::FlowSnapshot.repo(REPO).pr(3), move |hub| {
        let key = log::load_signing_key(&log::key_path(&at)).unwrap();
        let mut other = anchor::open(&at, key, hub, ANCHOR).unwrap();
        let config = crate::Config {
            reviewers: settings.flow_authors.iter().cloned().collect(),
            review_authors: settings.review_authors.iter().cloned().collect(),
            own_contexts: vec![crate::flow::SETTLED.into(), crate::train::CONTEXT.into()],
        };
        let before = other.store.head().map_or(0, |h| h.seq);
        crate::ingest::pull(&mut other.store, hub, &config, &REPO.parse().unwrap(), 3).unwrap();
        other.publish(hub).unwrap();
        theirs.set(Some(other.store.head().map_or(0, |h| h.seq) - before));
    });
    wrote
}

/// PR 3 recorded by a pass, pushed to, then passed twice more, with another
/// writer ingesting it during the second pass when `race`: the error each
/// pass met on PR 3, and what the ledger holds of it.
fn pushed(race: bool) -> (Vec<Option<String>>, (Vec<String>, String)) {
    let c = Case::new();
    let (ledger, mut a) = anchored(&c);
    let mut errors = vec![pass(&c, &mut a)];
    c.hub.push(REPO, 3, &super::head(4));
    c.hub.refresh_flow(REPO, 3);
    c.good_ci(&super::head(4));
    let wrote = race.then(|| race_pull_of_3(&c, ledger.path()));
    errors.push(pass(&c, &mut a));
    if let Some(wrote) = wrote {
        assert!(wrote.get().is_some_and(|n| n > 0), "{:?}", wrote.get());
    }
    errors.push(pass(&c, &mut a));
    (errors, pr_3(ledger.path()))
}

// PR 3 is already in the ledger when another writer records its new head
// while the pass reads it: the pass records only what's left, nothing
// twice, as it does with no other writer.
#[test]
fn a_pass_whose_new_head_another_writer_recorded_meanwhile_records_nothing_twice() {
    unblocked(120, || {
        let alone = pushed(false);
        assert_eq!(alone.0, [None, None, None]);
        assert_eq!(pushed(true), alone);
    });
}

// Another writer records the pass's own PR while the pass reads it: the
// pass, deciding from the ledger as it last wrote it, has its op refused,
// and that PR fails for the next pass, which reads what the other wrote
// and records the rest, nothing twice, as a pass with no other writer does.
#[test]
fn a_pass_whose_pr_another_writer_recorded_meanwhile_records_nothing_twice() {
    unblocked(120, || {
        let c = Case::new();
        let (alone, mut a) = anchored(&c);
        assert_eq!(pass(&c, &mut a), None);
        let (kinds, change) = pr_3(alone.path());

        let c = Case::new();
        let (raced, mut a) = anchored(&c);
        let wrote = race_pull_of_3(&c, raced.path());
        let refused = pass(&c, &mut a).expect("the pass's own op was refused");
        assert!(refused.contains("already in the ledger"), "{refused}");
        assert!(wrote.get().is_some_and(|n| n > 0), "{:?}", wrote.get());
        // What the other writer recorded, once, and nothing of the pass's.
        assert_eq!(kinds.last().map(String::as_str), Some("classify"));
        assert_eq!(pr_3(raced.path()).0, kinds[..kinds.len() - 1]);
        assert_eq!(pass(&c, &mut a), None);
        assert_eq!(pr_3(raced.path()), (kinds, change));
    });
}
