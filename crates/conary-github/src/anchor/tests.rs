use std::cell::RefCell;
use std::collections::HashMap;

use conary_core::ChangeId;
use conary_core::op::{Actor, ActorKind, Body, Repo};

use std::path::Path;

use super::*;

/// Refs as GitHub keeps them: each move a commit on the one before. With
/// `ledger` set, every read checks the ledger's lock is held, and with
/// `fail` set, the next move fails before it's made.
#[derive(Default)]
struct Fake {
    refs: RefCell<HashMap<String, Kept>>,
    moves: RefCell<usize>,
    ledger: RefCell<Option<std::path::PathBuf>>,
    fail: RefCell<bool>,
}

impl Anchors for Fake {
    fn get(&self, _: &str, name: &str) -> Result<Option<Kept>, ApiError> {
        if let Some(dir) = &*self.ledger.borrow() {
            let lock = std::fs::OpenOptions::new()
                .write(true)
                .open(dir.join("lock"))
                .expect("the lock file is there before the anchor is read");
            assert!(
                lock.try_lock().is_err(),
                "the anchor was read without the ledger's lock"
            );
        }
        Ok(self.refs.borrow().get(name).cloned())
    }

    fn set(
        &self,
        _: &str,
        name: &str,
        parent: Option<&str>,
        message: &str,
    ) -> Result<(), ApiError> {
        if std::mem::take(&mut *self.fail.borrow_mut()) {
            return Err(ApiError("injected".into()));
        }
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

fn key_path(dir: &Path) -> std::path::PathBuf {
    dir.join("key")
}

/// One write: open against the anchor, record, publish.
fn write(dir: &Path, f: &Fake, prs: &[u64]) -> Result<(), AnchorError> {
    let key = log::load_signing_key(&key_path(dir))?;
    let mut a = open(dir, key, f, REPO)?;
    for &pr in prs {
        observe(&mut a.store, pr);
    }
    a.publish(f)
}

/// The anchor on GitHub, as the ledger's writer reads it.
fn remote(f: &Fake) -> Remote {
    parse(&f.refs.borrow()[REF]).unwrap()
}

#[test]
fn every_write_checks_the_anchor_on_github_and_moves_it_on() {
    let dir = tempfile::tempdir().unwrap();
    let f = Fake {
        ledger: RefCell::new(Some(dir.path().to_owned())),
        ..Fake::default()
    };
    // Not set up: no key, and no anchor to write against.
    assert!(matches!(
        write(dir.path(), &f, &[1]),
        Err(AnchorError::Log(LogError::NoKey(_)))
    ));
    let made = init(dir.path(), &key_path(dir.path()), &f, REPO).unwrap();
    assert_eq!(made.anchor, None);
    assert_eq!(*f.moves.borrow(), 1);
    // Set up again: nothing changes.
    assert_eq!(
        init(dir.path(), &key_path(dir.path()), &f, REPO).unwrap(),
        made
    );
    assert_eq!(*f.moves.borrow(), 1);

    write(dir.path(), &f, &[1, 2]).unwrap();
    write(dir.path(), &f, &[3]).unwrap();
    assert_eq!(*f.moves.borrow(), 3);
    let head = remote(&f).anchor.unwrap();
    assert_eq!(head.seq, 3);
    // Readers have the same anchor beside the log.
    assert_eq!(log::load_anchor(dir.path()).unwrap(), Some(head));
    assert_eq!(log::read_anchored(dir.path(), &made.key).unwrap().len(), 3);
    // A write that records nothing leaves it be.
    write(dir.path(), &f, &[]).unwrap();
    assert_eq!(*f.moves.borrow(), 3);
    // Anyone can verify, so it takes no lock.
    f.ledger.take();
    assert_eq!(verify(dir.path(), &made.key, &f, REPO).unwrap(), Some(head));
}

#[test]
fn a_log_that_lost_ops_or_its_anchor_is_refused() {
    let f = Fake::default();
    let dir = tempfile::tempdir().unwrap();
    let key = init(dir.path(), &key_path(dir.path()), &f, REPO)
        .unwrap()
        .key;
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
        verify(dir.path(), &key, &f, REPO),
        Err(AnchorError::Log(LogError::Unanchored(_)))
    ));
    assert!(matches!(
        init(dir.path(), &key_path(dir.path()), &f, REPO),
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

    // Whole again, but the anchor on GitHub gone: not a new ledger, and
    // setting one up over it is refused too.
    std::fs::write(&log_file, &whole).unwrap();
    f.refs.borrow_mut().clear();
    assert!(matches!(
        write(dir.path(), &f, &[]),
        Err(AnchorError::Lost(_))
    ));
    assert!(matches!(
        verify(dir.path(), &key, &f, REPO),
        Err(AnchorError::Lost(_))
    ));
    assert!(matches!(
        init(dir.path(), &key_path(dir.path()), &f, REPO),
        Err(AnchorError::Lost(_))
    ));
    assert!(f.refs.borrow().is_empty());
}

#[test]
fn a_lost_key_or_directory_never_starts_another_ledger() {
    let f = Fake::default();
    let dir = tempfile::tempdir().unwrap();
    let first = init(dir.path(), &key_path(dir.path()), &f, REPO).unwrap();
    write(dir.path(), &f, &[1]).unwrap();
    let anchored = f.refs.borrow()[REF].clone();

    // The whole directory lost: setting up again needs the old key back.
    let fresh = tempfile::tempdir().unwrap();
    assert!(matches!(
        init(fresh.path(), &key_path(fresh.path()), &f, REPO),
        Err(AnchorError::Lost(why)) if why.contains("put its key back")
    ));
    assert!(!key_path(fresh.path()).exists());
    assert!(matches!(
        write(fresh.path(), &f, &[1]),
        Err(AnchorError::Log(LogError::NoKey(_)))
    ));
    // Another key, made by hand or on another machine: not this ledger's.
    std::fs::write(key_path(fresh.path()), [7u8; 32]).unwrap();
    assert!(matches!(
        init(fresh.path(), &key_path(fresh.path()), &f, REPO),
        Err(AnchorError::Lost(_))
    ));
    assert!(matches!(
        write(fresh.path(), &f, &[1]),
        Err(AnchorError::Lost(_))
    ));
    assert!(matches!(
        verify(
            dir.path(),
            &SigningKey::from_bytes(&[7; 32]).verifying_key(),
            &f,
            REPO
        ),
        Err(AnchorError::Lost(_))
    ));
    assert_eq!(f.refs.borrow()[REF], anchored);
    assert_eq!(remote(&f).key, first.key);
}

#[test]
fn a_set_up_or_write_that_stopped_partway_finishes_on_the_next_run() {
    let f = Fake::default();
    let dir = tempfile::tempdir().unwrap();
    // The key made, then the ref's creation failed.
    *f.fail.borrow_mut() = true;
    assert!(init(dir.path(), &key_path(dir.path()), &f, REPO).is_err());
    let key = log::load_signing_key(&key_path(dir.path())).unwrap();
    let made = init(dir.path(), &key_path(dir.path()), &f, REPO).unwrap();
    assert_eq!(made.key, key.verifying_key());

    // Ops written, then the anchor's move failed: the log holds the anchor
    // and more, so the next write takes it and moves the anchor past both.
    let mut a = open(dir.path(), key.clone(), &f, REPO).unwrap();
    observe(&mut a.store, 1);
    *f.fail.borrow_mut() = true;
    assert!(a.publish(&f).is_err());
    drop(a);
    assert_eq!(remote(&f).anchor, None);
    write(dir.path(), &f, &[2]).unwrap();
    assert_eq!(remote(&f).anchor.map(|a| a.seq), Some(2));
}

#[test]
fn a_refused_log_is_left_as_it_was() {
    let f = Fake::default();
    let dir = tempfile::tempdir().unwrap();
    init(dir.path(), &key_path(dir.path()), &f, REPO).unwrap();
    write(dir.path(), &f, &[1]).unwrap();
    // A torn last line, which a writer would cut, and no anchor on GitHub.
    let log_file = dir.path().join("ops.jsonl");
    let mut bytes = std::fs::read(&log_file).unwrap();
    bytes.extend_from_slice(b"{\"id\":\"");
    std::fs::write(&log_file, &bytes).unwrap();
    f.refs.borrow_mut().clear();
    assert!(write(dir.path(), &f, &[]).is_err());
    assert_eq!(std::fs::read(&log_file).unwrap(), bytes);
}

#[test]
fn an_anchor_commit_that_isnt_one_is_refused() {
    let f = Fake::default();
    let dir = tempfile::tempdir().unwrap();
    init(dir.path(), &key_path(dir.path()), &f, REPO).unwrap();
    for message in ["moved by hand", "key: 00\nanchor: none"] {
        f.refs.borrow_mut().insert(
            REF.into(),
            Kept {
                commit: "a".repeat(40),
                message: message.into(),
            },
        );
        assert!(matches!(
            write(dir.path(), &f, &[1]),
            Err(AnchorError::Lost(_))
        ));
    }
}
