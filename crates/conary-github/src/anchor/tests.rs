use std::cell::RefCell;
use std::collections::HashMap;

use conary_core::ChangeId;
use conary_core::op::{Actor, ActorKind, Body, Repo};

use super::*;

/// Refs as GitHub keeps them: each move a commit on the one before.
#[derive(Default)]
struct Fake {
    refs: RefCell<HashMap<String, Kept>>,
    moves: RefCell<usize>,
}

impl Anchors for Fake {
    fn get(&self, _: &str, name: &str) -> Result<Option<Kept>, ApiError> {
        Ok(self.refs.borrow().get(name).cloned())
    }

    fn set(
        &self,
        _: &str,
        name: &str,
        parent: Option<&str>,
        message: &str,
    ) -> Result<(), ApiError> {
        let mut refs = self.refs.borrow_mut();
        if refs.get(name).map(|k| k.commit.as_str()) != parent {
            return Err(ApiError(format!("{name} isn't at {parent:?}")));
        }
        *self.moves.borrow_mut() += 1;
        let commit = format!("{:040}", *self.moves.borrow());
        refs.insert(
            name.into(),
            Kept {
                commit,
                message: message.into(),
            },
        );
        Ok(())
    }
}

const REPO: &str = "FieldmouseWorks/remi";

fn key() -> SigningKey {
    SigningKey::from_bytes(&[9; 32])
}

fn observe(store: &mut Store, pr: u64) {
    store
        .record(Body::Observe {
            change: ChangeId::new().unwrap(),
            repo: Repo {
                owner: "FieldmouseWorks".into(),
                name: "burrow".into(),
            },
            pr,
            author: Actor {
                kind: ActorKind::Person,
                login: "TusanHomichi".into(),
                session: None,
            },
        })
        .unwrap();
}

/// One write: open against the anchor, record, publish.
fn write(dir: &std::path::Path, f: &Fake, prs: &[u64]) -> Result<(), AnchorError> {
    let mut a = open(dir, key(), f, REPO)?;
    for &pr in prs {
        observe(&mut a.store, pr);
    }
    a.publish(f, dir)
}

#[test]
fn every_write_checks_the_anchor_on_github_and_moves_it_on() {
    let dir = tempfile::tempdir().unwrap();
    let f = Fake::default();
    // First use: no anchor, no ops. Nothing written, nothing published.
    write(dir.path(), &f, &[]).unwrap();
    assert_eq!(*f.moves.borrow(), 0);
    write(dir.path(), &f, &[1, 2]).unwrap();
    write(dir.path(), &f, &[3]).unwrap();
    assert_eq!(*f.moves.borrow(), 2);
    let name = ref_name(&key().verifying_key());
    let kept = f.refs.borrow()[&name].clone();
    let head = parse(&kept).unwrap();
    assert_eq!(head.seq, 3);
    // Readers have the same anchor beside the log.
    assert_eq!(log::load_anchor(dir.path()).unwrap(), Some(head));
    assert_eq!(
        log::read_anchored(dir.path(), &key().verifying_key())
            .unwrap()
            .len(),
        3
    );
    // A write that records nothing leaves it be.
    write(dir.path(), &f, &[]).unwrap();
    assert_eq!(*f.moves.borrow(), 2);
}

#[test]
fn a_log_that_lost_ops_or_its_anchor_is_refused() {
    let f = Fake::default();
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), &f, &[1, 2]).unwrap();
    let log_file = dir.path().join("ops.jsonl");
    let whole = std::fs::read(&log_file).unwrap();

    // The log removed and started over, the local anchor with it: the one
    // on GitHub still names op 2.
    std::fs::remove_file(&log_file).unwrap();
    std::fs::remove_file(dir.path().join("anchor")).unwrap();
    assert!(matches!(
        write(dir.path(), &f, &[1]),
        Err(AnchorError::Store(StoreError::Log(LogError::Unanchored(_))))
    ));
    assert!(matches!(
        verify(dir.path(), &key().verifying_key(), &f, REPO),
        Err(AnchorError::Log(LogError::Unanchored(_)))
    ));

    // Its last op cut.
    let cut = whole[..whole.len() - 1]
        .iter()
        .rposition(|&b| b == b'\n')
        .unwrap();
    std::fs::write(&log_file, &whole[..=cut]).unwrap();
    assert!(matches!(
        write(dir.path(), &f, &[]),
        Err(AnchorError::Store(StoreError::Log(LogError::Unanchored(_))))
    ));

    // Whole again, but the anchor on GitHub gone: not a new ledger.
    std::fs::write(&log_file, &whole).unwrap();
    f.refs.borrow_mut().clear();
    assert!(matches!(
        write(dir.path(), &f, &[]),
        Err(AnchorError::Lost(_))
    ));
    assert!(matches!(
        verify(dir.path(), &key().verifying_key(), &f, REPO),
        Err(AnchorError::Lost(_))
    ));
}

#[test]
fn verify_keeps_the_anchor_on_github_for_readers() {
    let f = Fake::default();
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), &f, &[1]).unwrap();
    std::fs::remove_file(dir.path().join("anchor")).unwrap();
    let public = key().verifying_key();
    assert!(log::read_anchored(dir.path(), &public).is_err());
    let anchor = verify(dir.path(), &public, &f, REPO).unwrap();
    assert_eq!(anchor.map(|a| a.seq), Some(1));
    assert_eq!(log::read_anchored(dir.path(), &public).unwrap().len(), 1);
}

#[test]
fn an_anchor_commit_that_names_no_op_is_refused() {
    let f = Fake::default();
    let dir = tempfile::tempdir().unwrap();
    f.refs.borrow_mut().insert(
        ref_name(&key().verifying_key()),
        Kept {
            commit: "a".repeat(40),
            message: "moved by hand".into(),
        },
    );
    assert!(matches!(
        write(dir.path(), &f, &[1]),
        Err(AnchorError::Lost(_))
    ));
}
