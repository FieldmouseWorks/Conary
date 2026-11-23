//! Whether a merge was settled is one verdict, whichever writer records it
//! and whatever own contexts it was given: the train's status is never the
//! steward's word on it (Conary#574).
use super::flow_pass_case::{Case, REPO};
use super::flow_pass_lock::{anchored, moves, pass};
use crate::anchor;
use crate::sim::Call;
use crate::sim::STEWARD;
use crate::train::{CONTEXT, Mark};
use conary_core::log;
use std::cell::RefCell;
use std::rc::Rc;

// PR 3 joins the train on the steward's settled success, the train's newer
// pending standing on its head, and is merged by hand. The flow's pass and a
// sync, told the steward's context alone or the train's too, record the
// same op of that merge: a landing on the head the steward settled.
#[test]
fn a_merge_the_train_was_pending_on_is_one_verdict_for_every_writer() {
    crate::sim::unblocked(120, || {
        let mut c = Case::new();
        c.config.settings.train = true;
        c.clean_review();
        let (ledger, mut a) = anchored(&c);
        assert_eq!(pass(&c, &mut a), None);
        assert!(
            !(a.store.ledger().train(&REPO.parse().unwrap()).unwrap())
                .joined()
                .is_empty()
        );
        let head = super::head(3);
        // The train's pending is the steward's own login's, so a list that
        // holds its context would read it.
        assert_eq!(c.hub.latest(&head), Some(Mark::Waiting));
        assert!(c.config.settings.flow_authors.contains(STEWARD));

        // The steward's word stays the settled success: its write to withdraw
        // it fails.
        c.hub.fail(Call::FlowStatus);
        // Merged as the pass comes to read it: the pass records the merge.
        let merged: Rc<RefCell<Option<String>>> = Rc::default();
        let (theirs, at) = (Rc::clone(&merged), head.clone());
        c.hub.race(Call::FlowSnapshot.repo(REPO).pr(3), move |hub| {
            let message = crate::train::Message::new("by hand", 3, []);
            *theirs.borrow_mut() = Some(hub.squash(REPO, 3, &at, &message).unwrap());
        });
        let error = pass(&c, &mut a);
        let after = merged.borrow().clone().expect("PR 3 was merged");
        let flow = moves(ledger.path(), &after);
        assert_eq!(flow, ["land"], "the flow's pass: {error:?}");

        let settings = &c.config.settings;
        let lists = [
            vec![crate::flow::SETTLED.to_owned()],
            vec![crate::flow::SETTLED.to_owned(), CONTEXT.to_owned()],
        ];
        for (own_contexts, at) in lists
            .into_iter()
            .zip(["conary-test/sync", "conary-test/sync-2"])
        {
            // A ledger of its own, anchored apart from the flow's.
            let dir = tempfile::tempdir().unwrap();
            let key = log::key_path(dir.path());
            anchor::init(dir.path(), &key, &c.hub, at).unwrap();
            let signing = log::load_signing_key(&key).unwrap();
            let mut sync = anchor::open(dir.path(), signing, &c.hub, at).unwrap();
            let config = crate::Config {
                own_contexts: own_contexts.clone(),
                ..crate::flow::ledger::config(settings)
            };
            crate::ingest::pull(&mut sync.store, &c.hub, &config, &REPO.parse().unwrap(), 3)
                .unwrap();
            drop(sync);
            assert_eq!(
                moves(dir.path(), &after),
                flow,
                "a sync told {own_contexts:?}"
            );
        }
    });
}
