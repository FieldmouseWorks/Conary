//! Opening a writer against the anchor verifies the existing log prefix before
//! taking the writer lock, and checks anchor moves against that one pass rather
//! than re-reading the whole log under the lock.

use std::cell::Cell;

use super::*;

/// Wraps [`Fake`] to record `log::lines_read()` at the moment the anchor ref is
/// fetched — which happens under the ledger's lock (`Fake.ledger` checks it).
struct Measure<'a> {
    inner: &'a Fake,
    at_get: Cell<u64>,
}

impl<'a> Measure<'a> {
    fn new(inner: &'a Fake) -> Self {
        Self {
            inner,
            at_get: Cell::new(0),
        }
    }
}

impl Anchors for Measure<'_> {
    fn get(&self, repo: &str, name: &str) -> Result<Option<Kept>, ApiError> {
        let kept = self.inner.get(repo, name)?;
        self.at_get.set(log::lines_read());
        Ok(kept)
    }

    fn commit(&self, repo: &str, sha: &str) -> Result<Kept, ApiError> {
        self.inner.commit(repo, sha)
    }

    fn set(
        &self,
        repo: &str,
        name: &str,
        parent: Option<&str>,
        message: &str,
    ) -> Result<String, ApiError> {
        self.inner.set(repo, name, parent, message)
    }
}

#[test]
fn opening_verifies_the_prefix_before_taking_the_lock_and_stays_flat_as_the_log_grows() {
    let dir = tempfile::tempdir().unwrap();
    let f = Fake {
        ledger: RefCell::new(Some(dir.path().to_owned())),
        ..Fake::default()
    };
    init(dir.path(), &key_path(dir.path()), &f, REPO).unwrap();
    let key = log::load_signing_key(&key_path(dir.path())).unwrap();

    // 1x log (100 ops), then 2x log (200 ops): under the lock, neither reads
    // any line of the existing prefix.
    for target in [100u64, 200u64] {
        let mut a = open(dir.path(), key.clone(), &f, REPO).unwrap();
        let start = a.store.head().map_or(0, |h| h.seq as u64);
        for pr in (start + 1)..=target {
            observe(&mut a.store, pr);
        }
        a.publish(&f).unwrap();
        drop(a);

        let probe = Measure::new(&f);
        let before = log::lines_read();
        let opened = open(dir.path(), key.clone(), &probe, REPO).unwrap();
        let after = log::lines_read();
        let under_lock = after - probe.at_get.get();
        let total = after - before;

        assert_eq!(
            under_lock, 0,
            "at {target} ops, opening read {under_lock} lines under the writer lock"
        );
        assert_eq!(
            total, target,
            "at {target} ops, opening read {total} lines total instead of one pass"
        );
        assert_eq!(opened.store.head().map(|h| h.seq as u64), Some(target));
    }
}

#[test]
fn opening_with_a_stale_anchor_copy_checks_moves_without_rereading_under_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    let f = Fake {
        ledger: RefCell::new(Some(dir.path().to_owned())),
        ..Fake::default()
    };
    init(dir.path(), &key_path(dir.path()), &f, REPO).unwrap();
    let key = log::load_signing_key(&key_path(dir.path())).unwrap();

    // Write 50 ops and snapshot the local anchor copy there, then advance the
    // log and GitHub anchor through several more moves to 100 ops.
    let mut a = open(dir.path(), key.clone(), &f, REPO).unwrap();
    for pr in 1..=50u64 {
        observe(&mut a.store, pr);
    }
    a.publish(&f).unwrap();
    drop(a);
    let was = std::fs::read_to_string(log::anchor_path(dir.path())).unwrap();

    for batch in [51..=75u64, 76..=100u64] {
        let mut a = open(dir.path(), key.clone(), &f, REPO).unwrap();
        for pr in batch {
            observe(&mut a.store, pr);
        }
        a.publish(&f).unwrap();
    }
    stale_copy(dir.path(), &was);

    let probe = Measure::new(&f);
    let before = log::lines_read();
    let opened = open(dir.path(), key.clone(), &probe, REPO).unwrap();
    let after = log::lines_read();
    let under_lock = after - probe.at_get.get();
    let total = after - before;

    assert_eq!(
        under_lock, 0,
        "stale-anchor open read {under_lock} lines under the writer lock"
    );
    assert_eq!(
        total, 100,
        "stale-anchor open read {total} lines total (double read?)"
    );
    assert_eq!(opened.store.head().map(|h| h.seq), Some(100));
    assert_eq!(
        log::load_checked(dir.path()).unwrap().at.as_deref(),
        Some(f.refs.borrow()[REF].commit.as_str())
    );
}

#[test]
fn opening_with_a_corrupt_log_still_refuses_a_missing_anchor_first() {
    let dir = tempfile::tempdir().unwrap();
    let f = Fake::default();
    init(dir.path(), &key_path(dir.path()), &f, REPO).unwrap();
    write(dir.path(), &f, &[1, 2]).unwrap();

    let log_file = log::log_path(dir.path());
    let mut corrupt = std::fs::read(&log_file).unwrap();
    corrupt[10] ^= 1;
    std::fs::write(&log_file, &corrupt).unwrap();

    let saved = f.refs.borrow_mut().remove(REF).unwrap();
    let key = log::load_signing_key(&key_path(dir.path())).unwrap();
    assert!(matches!(
        open(dir.path(), key.clone(), &f, REPO),
        Err(AnchorError::Lost(_))
    ));
    assert_eq!(std::fs::read(&log_file).unwrap(), corrupt);

    f.refs.borrow_mut().insert(REF.into(), saved);
    assert!(matches!(
        open(dir.path(), key, &f, REPO),
        Err(AnchorError::Store(StoreError::Log(LogError::Corrupt {
            line: 1,
            ..
        })))
    ));
    assert_eq!(std::fs::read(&log_file).unwrap(), corrupt);
}
