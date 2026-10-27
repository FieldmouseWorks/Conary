use std::cell::RefCell;
use std::collections::HashMap;

use conary_core::ChangeId;
use conary_core::op::{Actor, ActorKind, Body, Repo};

use std::path::Path;

use super::*;

/// Refs as GitHub keeps them: each move a commit on the one before, kept in
/// `commits`. With `ledger` set, every read checks the ledger's lock is held;
/// with `fail` set, the next move fails before it's made; and with
/// `write_after_get` set, a whole write to that ledger runs just after the
/// next read of the ref.
#[derive(Default)]
struct Fake {
    refs: RefCell<HashMap<String, Kept>>,
    commits: RefCell<HashMap<String, Kept>>,
    moves: RefCell<usize>,
    ledger: RefCell<Option<std::path::PathBuf>>,
    fail: RefCell<bool>,
    write_after_get: RefCell<Option<std::path::PathBuf>>,
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
        let kept = self.refs.borrow().get(name).cloned();
        if let Some(dir) = self.write_after_get.take() {
            write(&dir, self, &[100]).unwrap();
        }
        Ok(kept)
    }

    fn commit(&self, _: &str, sha: &str) -> Result<Kept, ApiError> {
        self.commits
            .borrow()
            .get(sha)
            .cloned()
            .ok_or_else(|| ApiError(format!("no commit {sha}")))
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
        let kept = Kept {
            commit: format!("{:040}", *self.moves.borrow()),
            message: message.into(),
            parent: parent.map(str::to_owned),
        };
        self.commits
            .borrow_mut()
            .insert(kept.commit.clone(), kept.clone());
        refs.insert(name.into(), kept);
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
        Err(AnchorError::Log(LogError::NoAnchor(_)))
    ));
    assert!(matches!(
        verify(dir.path(), &key, &f, REPO),
        Err(AnchorError::Log(LogError::Unanchored(_)))
    ));
    assert!(matches!(
        init(dir.path(), &key_path(dir.path()), &f, REPO),
        Err(AnchorError::Log(LogError::Unanchored(_)))
    ));

    // Its last op cut, the copy beside it intact.
    let head = remote(&f).anchor.unwrap();
    std::fs::write(dir.path().join("anchor"), format!("{head}\n")).unwrap();
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
                parent: None,
            },
        );
        assert!(matches!(
            write(dir.path(), &f, &[1]),
            Err(AnchorError::Lost(_))
        ));
    }
}

#[test]
fn an_anchor_moved_back_on_github_is_refused() {
    let f = Fake::default();
    let dir = tempfile::tempdir().unwrap();
    let key = init(dir.path(), &key_path(dir.path()), &f, REPO)
        .unwrap()
        .key;
    write(dir.path(), &f, &[1, 2]).unwrap();
    let log_file = dir.path().join("ops.jsonl");
    let whole = std::fs::read(&log_file).unwrap();
    let cut = whole[..whole.len() - 1]
        .iter()
        .rposition(|&b| b == b'\n')
        .unwrap();
    let one = Anchor {
        seq: 1,
        op: log::read(dir.path(), &key, None).unwrap()[0].id,
    };
    // The log cut to its first op, and a fast-forward on GitHub naming that
    // op, or none: the copy beside the log still names op 2.
    std::fs::write(&log_file, &whole[..=cut]).unwrap();
    for back in [Some(one), None] {
        let parent = f.refs.borrow()[REF].commit.clone();
        f.set(REPO, REF, Some(&parent), &message(&key, back))
            .unwrap();
        assert!(matches!(
            write(dir.path(), &f, &[]),
            Err(AnchorError::Lost(why)) if why.contains("last checked at op 2")
        ));
        assert!(matches!(
            verify(dir.path(), &key, &f, REPO),
            Err(AnchorError::Lost(_))
        ));
        assert!(matches!(
            init(dir.path(), &key_path(dir.path()), &f, REPO),
            Err(AnchorError::Lost(_))
        ));
    }
    assert_eq!(std::fs::read(&log_file).unwrap(), &whole[..=cut]);
}

/// Puts the copy beside the log back to what it said before a write: a write
/// that moved the ref and stopped before it kept the copy.
fn stale_copy(dir: &Path, was: &str) {
    std::fs::write(dir.join("anchor"), was).unwrap();
}

#[test]
fn a_write_that_moved_the_ref_but_kept_no_copy_is_caught_up() {
    let f = Fake::default();
    let dir = tempfile::tempdir().unwrap();
    let key = init(dir.path(), &key_path(dir.path()), &f, REPO)
        .unwrap()
        .key;
    write(dir.path(), &f, &[1, 2]).unwrap();
    let was = std::fs::read_to_string(dir.path().join("anchor")).unwrap();
    write(dir.path(), &f, &[3]).unwrap();
    write(dir.path(), &f, &[4]).unwrap();
    stale_copy(dir.path(), &was);
    assert_eq!(
        verify(dir.path(), &key, &f, REPO).unwrap(),
        remote(&f).anchor
    );
    write(dir.path(), &f, &[5]).unwrap();
    assert_eq!(remote(&f).anchor.map(|a| a.seq), Some(5));
    assert_eq!(log::load_anchor(dir.path()).unwrap(), remote(&f).anchor);
    // So does setting up again with the copy stale, or gone.
    stale_copy(dir.path(), &was);
    init(dir.path(), &key_path(dir.path()), &f, REPO).unwrap();
    std::fs::remove_file(dir.path().join("anchor")).unwrap();
    init(dir.path(), &key_path(dir.path()), &f, REPO).unwrap();
    assert_eq!(log::load_anchor(dir.path()).unwrap(), remote(&f).anchor);

    // A copy far behind is from somewhere else; with it gone, setting up
    // again checks the whole history instead.
    for pr in 6..6 + MAX_AHEAD as u64 + 1 {
        write(dir.path(), &f, &[pr]).unwrap();
    }
    stale_copy(dir.path(), &was);
    assert!(matches!(
        write(dir.path(), &f, &[]),
        Err(AnchorError::Lost(why)) if why.contains("moves ahead")
    ));
    std::fs::remove_file(dir.path().join("anchor")).unwrap();
    init(dir.path(), &key_path(dir.path()), &f, REPO).unwrap();
    write(dir.path(), &f, &[]).unwrap();
}

/// A ledger whose ref moved to op 3 after the copy beside the log named op
/// 2: the log as it was at op 2, and the copy then.
struct Published {
    f: Fake,
    dir: tempfile::TempDir,
    key: VerifyingKey,
    two: Vec<u8>,
    was: String,
}

impl Published {
    fn new() -> Self {
        let f = Fake::default();
        let dir = tempfile::tempdir().unwrap();
        let key = init(dir.path(), &key_path(dir.path()), &f, REPO)
            .unwrap()
            .key;
        write(dir.path(), &f, &[1, 2]).unwrap();
        let two = std::fs::read(dir.path().join("ops.jsonl")).unwrap();
        let was = std::fs::read_to_string(dir.path().join("anchor")).unwrap();
        write(dir.path(), &f, &[3]).unwrap();
        stale_copy(dir.path(), &was);
        Self {
            f,
            dir,
            key,
            two,
            was,
        }
    }

    fn at_two(&self) -> Anchor {
        self.was.trim().parse().unwrap()
    }

    /// The log put back to op 2 and taken on to op 5 with the ledger's own
    /// key, in place of the op 3 that was published.
    fn rewrite(&self) -> Option<Anchor> {
        std::fs::write(self.dir.path().join("ops.jsonl"), &self.two).unwrap();
        let signing = log::load_signing_key(&key_path(self.dir.path())).unwrap();
        let lock = Lock::take(self.dir.path()).unwrap();
        let mut store = Store::open(lock, signing, Some(self.at_two())).unwrap();
        for pr in [30, 40, 50] {
            observe(&mut store, pr);
        }
        store.head()
    }

    /// A fast-forward of the ref, by whoever can push to it.
    fn moved(&self, to: Option<Anchor>) {
        let parent = self.f.refs.borrow()[REF].commit.clone();
        self.f
            .set(REPO, REF, Some(&parent), &message(&self.key, to))
            .unwrap();
    }

    fn refused(&self, why: &str) {
        let (dir, f) = (self.dir.path(), &self.f);
        for (what, r) in [
            ("write", write(dir, f, &[])),
            ("verify", verify(dir, &self.key, f, REPO).map(|_| ())),
            ("init", init(dir, &key_path(dir), f, REPO).map(|_| ())),
        ] {
            assert!(
                matches!(
                    r,
                    Err(AnchorError::Log(LogError::Unanchored(_)))
                        | Err(AnchorError::Store(StoreError::Log(LogError::Unanchored(_))))
                        | Err(AnchorError::Lost(_))
                ),
                "{why}: {what} gave {r:?}"
            );
        }
    }
}

#[test]
fn a_history_that_drops_an_acknowledged_op_is_refused() {
    // The log cut back to op 2, and a fast-forward naming op 2 again: op 3
    // was published, so it's refused.
    let p = Published::new();
    std::fs::write(p.dir.path().join("ops.jsonl"), &p.two).unwrap();
    p.moved(Some(p.at_two()));
    p.refused("op 2 named again");

    // Op 3 rewritten and the log taken further, and the ref fast-forwarded
    // to it: the move naming the first op 3 is still in the ref's history,
    // and the log no longer holds it.
    let p = Published::new();
    let rewritten = p.rewrite();
    p.moved(rewritten);
    stale_copy(p.dir.path(), &p.was);
    p.refused("a longer history in place of op 3");
    let refused = |r: Result<(), AnchorError>| {
        assert!(
            matches!(r, Err(AnchorError::Log(LogError::Unanchored(_)))),
            "{r:?}"
        );
    };
    // Each on its own: a write with the copy stale, and with no copy, set
    // up again or verified, the whole history checked.
    refused(write(p.dir.path(), &p.f, &[]));
    std::fs::remove_file(p.dir.path().join("anchor")).unwrap();
    refused(init(p.dir.path(), &key_path(p.dir.path()), &p.f, REPO).map(|_| ()));
    refused(verify(p.dir.path(), &p.key, &p.f, REPO).map(|_| ()));
}

#[test]
fn verify_beside_a_writer_never_reports_a_false_loss() {
    let f = Fake::default();
    let dir = tempfile::tempdir().unwrap();
    let key = init(dir.path(), &key_path(dir.path()), &f, REPO)
        .unwrap()
        .key;
    write(dir.path(), &f, &[1]).unwrap();
    // A writer finishes a whole write just after verify reads the ref.
    *f.write_after_get.borrow_mut() = Some(dir.path().to_owned());
    let seen = verify(dir.path(), &key, &f, REPO).unwrap();
    assert_eq!(seen.map(|a| a.seq), Some(1));
    assert_eq!(remote(&f).anchor.map(|a| a.seq), Some(2));
}

#[test]
fn setting_up_again_puts_back_a_lost_or_wrong_public_key() {
    let f = Fake::default();
    let dir = tempfile::tempdir().unwrap();
    let key = init(dir.path(), &key_path(dir.path()), &f, REPO)
        .unwrap()
        .key;
    write(dir.path(), &f, &[1]).unwrap();
    let public = log::public_path(&key_path(dir.path()));
    let right = std::fs::read(&public).unwrap();
    let secret = std::fs::read(key_path(dir.path())).unwrap();
    for broken in [None, Some("00\n"), Some(&*"ab".repeat(32))] {
        match broken {
            None => std::fs::remove_file(&public).unwrap(),
            Some(text) => std::fs::write(&public, text).unwrap(),
        }
        init(dir.path(), &key_path(dir.path()), &f, REPO).unwrap();
        assert_eq!(std::fs::read(&public).unwrap(), right);
    }
    assert_eq!(std::fs::read(key_path(dir.path())).unwrap(), secret);
    assert_eq!(remote(&f).key, key);
    assert_eq!(log::read_anchored(dir.path(), &key).unwrap().len(), 1);
}
