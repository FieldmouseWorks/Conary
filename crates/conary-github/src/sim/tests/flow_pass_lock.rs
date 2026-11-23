//! A flow pass that writes to the ledger holds its lock only while it
//! writes: a read of GitHub that hangs holds up no other writer, and what
//! the other writer recorded and anchored meanwhile is kept by both.
use super::flow_pass_case::{Case, REPO};
use crate::anchor;
use crate::api::Source;
use crate::sim::{Call, unblocked};
use conary_core::ChangeId;
use conary_core::log;
use conary_core::op::{Actor, ActorKind, Body};
use std::cell::{Cell, RefCell};
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
pub(super) fn anchored(c: &Case) -> (tempfile::TempDir, anchor::Anchored) {
    let ledger = tempfile::tempdir().unwrap();
    let dir = ledger.path();
    anchor::init(dir, &log::key_path(dir), &c.hub, ANCHOR).unwrap();
    let key = log::load_signing_key(&log::key_path(dir)).unwrap();
    let anchored = anchor::open(dir, key, &c.hub, ANCHOR).unwrap();
    (ledger, anchored)
}

/// One pass of `c`, published after; the error its PR 3 met, if any.
pub(super) fn pass(c: &Case, anchored: &mut anchor::Anchored) -> Option<String> {
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
/// before the pass's next `call`: the ops it recorded, once it has.
fn race_pull_of_3(c: &Case, dir: &Path, call: crate::sim::On) -> Rc<Cell<Option<usize>>> {
    let wrote: Rc<Cell<Option<usize>>> = Rc::default();
    let (theirs, at) = (Rc::clone(&wrote), dir.to_owned());
    let settings = c.config.settings.clone();
    c.hub.race(call, move |hub| {
        let key = log::load_signing_key(&log::key_path(&at)).unwrap();
        let mut other = anchor::open(&at, key, hub, ANCHOR).unwrap();
        let config = crate::flow::ledger::config(&settings);
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
    let wrote =
        race.then(|| race_pull_of_3(&c, ledger.path(), Call::FlowSnapshot.repo(REPO).pr(3)));
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
        // Between the pass's read of the PR and its recording of it.
        let wrote = race_pull_of_3(&c, raced.path(), Call::MergeBase.repo(REPO));
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

/// Another writer raising the case's PR 3 to R2 in the ledger at `dir`, as
/// `cn class` records a raise (or a policy's R2, with no label), just before
/// the pass's first `call` once the pass has classified PR 3.
fn race_raise_of_3(c: &Case, dir: &Path, call: Call, label: bool) -> Rc<Cell<bool>> {
    let raised: Rc<Cell<bool>> = Rc::default();
    arm_raise(&c.hub, dir.to_owned(), call, label, Rc::clone(&raised));
    raised
}

fn arm_raise(
    hub: &crate::sim::Hub,
    at: std::path::PathBuf,
    call: Call,
    label: bool,
    raised: Rc<Cell<bool>>,
) {
    hub.race(call.repo(REPO), move |hub| {
        let public = log::load_public_key(&log::public_path(&log::key_path(&at))).unwrap();
        let classified = conary_core::reader::read(&at, &public)
            .unwrap()
            .by_pr(&REPO.parse().unwrap(), 3)
            .is_some_and(|c| !c.classes.is_empty());
        if !classified {
            return arm_raise(hub, at, call, label, raised);
        }
        let key = log::load_signing_key(&log::key_path(&at)).unwrap();
        let mut other = anchor::open(&at, key, hub, ANCHOR).unwrap();
        let change = other
            .store
            .ledger()
            .by_pr(&REPO.parse().unwrap(), 3)
            .unwrap();
        let said = change.classes.last().unwrap().clone();
        let body = Body::Classify {
            change: change.id,
            revision: said.revision,
            class: conary_core::Class::R2,
            because: Some("it's R2".into()),
            docs: false,
            policy: said.policy,
            raised: if label {
                vec![conary_core::policy::R2_LABEL.into()]
            } else {
                vec![]
            },
        };
        other.store.record(body).unwrap();
        other.publish(hub).unwrap();
        raised.set(true);
    });
}

// Another writer raises the pass's PR to R2 after the pass classified it and
// before it merges: the pass's last looks read the ledger as it stands, and
// from the last one to the merge it holds the lock, so it never merges on
// the class it read before and takes its permission back.
#[test]
fn a_pass_never_merges_a_pr_another_writer_raised_meanwhile() {
    unblocked(120, || {
        // Raised before the permission's last look, or after it and before
        // the merge.
        for (call, label) in [
            (Call::Statuses, true),
            (Call::CompareCommits, true),
            // R2 by a policy, so no label's raise says it.
            (Call::CompareCommits, false),
        ] {
            let c = Case::new();
            c.clean_review();
            let (ledger, mut a) = anchored(&c);
            let raised = race_raise_of_3(&c, ledger.path(), call, label);
            let error = pass(&c, &mut a);
            assert!(raised.get(), "{call:?}");
            assert!(error.is_some(), "{call:?}");
            assert_eq!(c.hub.merged(REPO, 3), None, "{call:?}: {error:?}");
            let settled = c
                .hub
                .statuses(REPO, &super::head(3))
                .unwrap()
                .into_iter()
                .find(|s| s.context == crate::flow::SETTLED)
                .unwrap();
            assert_ne!(settled.state, "success", "{call:?}");
        }
    });
}

/// The case's PR 3 pushed to head 4, and recorded so by another writer into
/// the ledger at `at`.
fn push_recorded_by_another(hub: &crate::sim::Hub, at: &Path, settings: &crate::flow::Settings) {
    hub.push(REPO, 3, &super::head(4));
    let key = log::load_signing_key(&log::key_path(at)).unwrap();
    let mut other = anchor::open(at, key, hub, ANCHOR).unwrap();
    let config = crate::flow::ledger::config(settings);
    crate::ingest::pull(&mut other.store, hub, &config, &REPO.parse().unwrap(), 3).unwrap();
    other.publish(hub).unwrap();
}

/// The heads the ledger at `dir` recorded of any PR, in order.
fn heads(dir: &Path) -> Vec<String> {
    let public = log::load_public_key(&log::public_path(&log::key_path(dir))).unwrap();
    log::read(dir, &public, None)
        .unwrap()
        .iter()
        .filter_map(|o| match &o.op.body {
            Body::Revision { head, .. } => Some(head.to_string()),
            _ => None,
        })
        .collect()
}

// A pass read PR 3 at head 3 to record it; while it reads the rest, the PR
// is pushed to and another writer records head 4. Head 3 is never recorded
// after it: the pass's op is refused and the next pass reads again.
#[test]
fn a_pass_never_records_a_pr_as_it_was_before_what_another_writer_recorded() {
    unblocked(120, || {
        // The first of the reads between the PR's and its recording, a middle
        // one, and one after that.
        for calls in [
            &[Call::MergeBase][..],
            &[Call::Timeline],
            &[Call::Timeline, Call::Reviews],
        ] {
            let c = Case::new();
            let (ledger, mut a) = anchored(&c);
            assert_eq!(pass(&c, &mut a), None);
            let raced: Rc<Cell<bool>> = Rc::default();
            let (theirs, at) = (Rc::clone(&raced), ledger.path().to_owned());
            let settings = c.config.settings.clone();
            let record = move |hub: &crate::sim::Hub| {
                push_recorded_by_another(hub, &at, &settings);
                theirs.set(true);
            };
            match *calls {
                [call] => c.hub.race(call.repo(REPO), record),
                [first, then] => c.hub.race(first.repo(REPO), move |hub| {
                    hub.race(then.repo(REPO), record)
                }),
                _ => unreachable!("one read, or one after another"),
            }
            let error = pass(&c, &mut a);
            assert!(raced.get(), "{calls:?}");
            let both = [super::head(3), super::head(4)];
            assert_eq!(heads(ledger.path()), both, "{calls:?}");
            let error = error.expect("the pass's older op was refused");
            assert!(error.contains("read it again"), "{calls:?}: {error}");
            c.hub.refresh_flow(REPO, 3);
            c.good_ci(&super::head(4));
            assert_eq!(pass(&c, &mut a), None, "{calls:?}");
            assert_eq!(heads(ledger.path()), both, "{calls:?}");
        }
    });
}

// Another writer seats the pass's PR after the pass's last write: the step at
// the pass's end reads the train as it stands under the lock it lands under,
// so who's seated, and whose failed decision keeps it from stepping, is
// never read from before.
#[test]
fn a_passs_step_reads_the_train_as_it_stands() {
    unblocked(120, || {
        let mut c = Case::new();
        c.config.settings.train = true;
        let (ledger, mut a) = anchored(&c);
        let at = ledger.path().to_owned();
        let settings = c.config.settings.clone();
        // The pass waits for a review: the read in its scope's look after it
        // staged the PR follows the store's last catch-up, and nothing is
        // recorded after it.
        let seat = move |hub: &crate::sim::Hub| {
            let key = log::load_signing_key(&log::key_path(&at)).unwrap();
            let mut other = anchor::open(&at, key, hub, ANCHOR).unwrap();
            let config = crate::flow::ledger::config(&settings);
            let joined = crate::train::join(
                &mut other.store,
                hub,
                hub,
                &config,
                &REPO.parse().unwrap(),
                3,
                &super::head(3),
                &[],
                &conary_core::Policy::parse(b"{}").unwrap(),
                None,
            )
            .unwrap();
            assert_eq!(joined, crate::train::Joined::Joined);
            other.publish(hub).unwrap();
        };
        c.hub.race(Call::FlowStage.repo(REPO), move |hub| {
            hub.race(Call::FlowSnapshot.repo(REPO), seat)
        });
        assert_eq!(pass(&c, &mut a), None);
        let train = a.store.ledger().train(&REPO.parse().unwrap()).unwrap();
        assert!(train.in_flight().is_some(), "the step missed the PR seated");
    });
}

/// How the ledger at `dir` says trunk moved to `after`, each time it does.
pub(super) fn moves(dir: &Path, after: &str) -> Vec<&'static str> {
    let public = log::load_public_key(&log::public_path(&log::key_path(dir))).unwrap();
    log::read(dir, &public, None)
        .unwrap()
        .iter()
        .filter_map(|o| match &o.op.body {
            Body::Land { after: to, .. } if to.to_string() == after => Some("land"),
            Body::Outside { after: to, .. } if to.to_string() == after => Some("outside"),
            _ => None,
        })
        .collect()
}

// PR 3, never settled, is merged by hand. Another writer records that merge
// as a landing (one that trusts another steward login, say) while the pass is
// judging it outside: the landing stands alone.
#[test]
fn a_pass_never_records_outside_where_another_writer_recorded_a_land() {
    unblocked(120, || {
        let c = Case::new();
        let (ledger, mut a) = anchored(&c);
        assert_eq!(pass(&c, &mut a), None);

        let after: Rc<RefCell<Option<String>>> = Rc::default();
        let (merged, at) = (Rc::clone(&after), ledger.path().to_owned());
        // Merged before the pass reads it to record it, and recorded while
        // the pass reads the merge's commit.
        c.hub.race(Call::FlowSnapshot.repo(REPO).pr(3), move |hub| {
            hub.race(Call::Pull.repo(REPO).pr(3), move |hub| {
                let base = hub.pull_json(REPO, 3)["base"]["ref"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                let before = hub.branch_at(REPO, &base).unwrap();
                let message = crate::train::Message::new("by hand", 3, []);
                let sha = hub.squash(REPO, 3, &super::head(3), &message).unwrap();
                *merged.borrow_mut() = Some(sha.clone());
                hub.race(Call::CommitPulls.repo(REPO), move |hub| {
                    let key = log::load_signing_key(&log::key_path(&at)).unwrap();
                    let mut other = anchor::open(&at, key, hub, ANCHOR).unwrap();
                    let change = other
                        .store
                        .ledger()
                        .by_pr(&REPO.parse().unwrap(), 3)
                        .unwrap();
                    let body = Body::Land {
                        change: change.id,
                        revision: change.revisions.last().unwrap().id,
                        before: before.parse().unwrap(),
                        after: sha.parse().unwrap(),
                        by: conary_core::op::Authority::Status {
                            login: "another-steward[bot]".into(),
                        },
                    };
                    other.store.record(body).unwrap();
                    other.publish(hub).unwrap();
                });
            });
        });
        let error = pass(&c, &mut a);
        let after = after.borrow().clone().expect("PR 3 was merged");
        assert_eq!(moves(ledger.path(), &after), ["land"], "{error:?}");
        let error = error.expect("the pass's outside was refused");
        assert!(error.contains("read it again"), "{error}");
        assert_eq!(pass(&c, &mut a), None);
        assert_eq!(moves(ledger.path(), &after), ["land"]);
    });
}

// The other order: another writer records a merge outside, and stops before
// it closes the change, while the pass is judging that merge a landing. The
// violation stands alone.
#[test]
fn a_pass_never_records_a_land_where_another_writer_recorded_outside() {
    unblocked(120, || {
        let c = Case::new();
        let (ledger, mut a) = anchored(&c);
        assert_eq!(pass(&c, &mut a), None);

        let after: Rc<RefCell<Option<String>>> = Rc::default();
        let (merged, at) = (Rc::clone(&after), ledger.path().to_owned());
        c.hub.race(Call::FlowSnapshot.repo(REPO).pr(3), move |hub| {
            hub.race(Call::Pull.repo(REPO).pr(3), move |hub| {
                let sha = hub.steward_lands(REPO, 3);
                *merged.borrow_mut() = Some(sha.clone());
                hub.race(Call::CommitPulls.repo(REPO), move |hub| {
                    let key = log::load_signing_key(&log::key_path(&at)).unwrap();
                    let mut other = anchor::open(&at, key, hub, ANCHOR).unwrap();
                    let body = Body::Outside {
                        repo: REPO.parse().unwrap(),
                        before: None,
                        after: sha.parse().unwrap(),
                    };
                    other.store.record(body).unwrap();
                    other.publish(hub).unwrap();
                });
            });
        });
        let error = pass(&c, &mut a);
        let after = after.borrow().clone().expect("PR 3 was merged");
        assert_eq!(moves(ledger.path(), &after), ["outside"], "{error:?}");
        let error = error.expect("the pass's land was refused");
        assert!(error.contains("read it again"), "{error}");
        assert_eq!(pass(&c, &mut a), None);
        assert_eq!(moves(ledger.path(), &after), ["outside"]);
    });
}
