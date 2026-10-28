//! Reading the ledger: once, as `cn` does, or kept in memory and read again
//! only when the log has changed, as conaryd does.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ed25519_dalek::VerifyingKey;

use crate::ledger::{Invalid, Ledger};
use crate::log::{self, LogError};

#[derive(Debug)]
pub enum ReadError {
    Log(LogError),
    /// The log verifies, but an op in it doesn't apply.
    Invalid(String),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Log(e) => e.fmt(f),
            Self::Invalid(e) => f.write_str(e),
        }
    }
}

impl std::error::Error for ReadError {}

impl From<LogError> for ReadError {
    fn from(e: LogError) -> Self {
        Self::Log(e)
    }
}

/// The log's and the anchor copy's file, size and change time: when
/// neither moved, neither did the ledger. The change time is the kernel's,
/// moved by every write and every change of times, and no writer sets it
/// back, so an edit that keeps the size and puts the modification time back
/// still moves it. The anchor copy is renamed into place, so a new one is
/// a new file as well.
type Stamp = [Option<(u64, u64, i64, i64)>; 2];

pub struct Reader {
    dir: PathBuf,
    key: VerifyingKey,
    ledger: Arc<Ledger>,
    stamp: Stamp,
}

fn stamp(dir: &Path) -> Stamp {
    [log::log_path(dir), log::anchor_path(dir)].map(|path| {
        fs::metadata(path)
            .ok()
            .map(|m| (m.ino(), m.len(), m.ctime(), m.ctime_nsec()))
    })
}

/// The ledger at `dir`, verified against `key` and the anchor beside it.
pub fn read(dir: &Path, key: &VerifyingKey) -> Result<Ledger, ReadError> {
    let ops = log::read_anchored(dir, key)?;
    Ledger::replay(&ops).map_err(|(id, e): (_, Invalid)| {
        ReadError::Invalid(format!("op {id} in the log doesn't apply: {e}"))
    })
}

impl Reader {
    /// The ledger at `dir`, verified against `key` and the anchor beside it.
    pub fn open(dir: &Path, key: VerifyingKey) -> Result<Self, ReadError> {
        let stamp = stamp(dir);
        let ledger = Arc::new(read(dir, &key)?);
        Ok(Self {
            dir: dir.to_owned(),
            key,
            ledger,
            stamp,
        })
    }

    /// The ledger as the log now holds it, read again only if the log or
    /// the anchor beside it changed since the last read. A read that fails
    /// keeps nothing of it: the next one tries again. What it returns is a
    /// snapshot, so a call can go on reading it while the next refresh
    /// reads the log again.
    pub fn refresh(&mut self) -> Result<Arc<Ledger>, ReadError> {
        let now = stamp(&self.dir);
        if now != self.stamp {
            self.ledger = Arc::new(read(&self.dir, &self.key)?);
            self.stamp = now;
        }
        Ok(Arc::clone(&self.ledger))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;
    use crate::id::ChangeId;
    use crate::op::{Actor, ActorKind, Body};

    fn observe(store: &mut Store, pr: u64) {
        store
            .record(Body::Observe {
                change: ChangeId::new().unwrap(),
                repo: "o/r".parse().unwrap(),
                pr,
                author: Actor {
                    kind: ActorKind::Person,
                    login: "someone".into(),
                    session: None,
                },
            })
            .unwrap();
    }

    #[test]
    fn a_refresh_reads_what_was_written_since_and_keeps_the_old_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let key = ed25519_dalek::SigningKey::from_bytes(&[4; 32]);
        let public = key.verifying_key();
        let lock = log::Lock::take(dir.path()).unwrap();
        log::save_anchor(&lock, None, "c0").unwrap();
        let mut store = Store::open(lock, key, None).unwrap();
        observe(&mut store, 1);
        let mut reader = Reader::open(dir.path(), public).unwrap();
        let before = reader.refresh().unwrap();
        assert_eq!(before.changes().count(), 1);
        observe(&mut store, 2);
        let after = reader.refresh().unwrap();
        assert_eq!(after.changes().count(), 2);
        // A call still reading the old snapshot isn't changed under it.
        assert_eq!(before.changes().count(), 1);
        // Nothing written, nothing read: the same snapshot.
        assert!(Arc::ptr_eq(&after, &reader.refresh().unwrap()));
    }

    #[test]
    fn a_new_anchor_copy_is_read_even_when_its_size_and_time_are_the_old_ones() {
        let dir = tempfile::tempdir().unwrap();
        let key = ed25519_dalek::SigningKey::from_bytes(&[4; 32]);
        let public = key.verifying_key();
        let lock = log::Lock::take(dir.path()).unwrap();
        log::save_anchor(&lock, None, "c0").unwrap();
        let mut store = Store::open(lock, key, None).unwrap();
        observe(&mut store, 1);
        let head = store.head().unwrap();
        log::save_anchor(store.lock(), Some(head), "c1").unwrap();
        let mut reader = Reader::open(dir.path(), public).unwrap();
        // An anchor naming an op the log doesn't hold, in a copy the same
        // size and with the same time as the one it replaces.
        let path = log::anchor_path(dir.path());
        let was = fs::metadata(&path).unwrap().modified().unwrap();
        let text = head.to_string();
        let last = if text.ends_with('0') { "1" } else { "0" };
        let other: log::Anchor = format!("{}{last}", &text[..text.len() - 1])
            .parse()
            .unwrap();
        log::save_anchor(store.lock(), Some(other), "c1").unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(was)
            .unwrap();
        let got = reader.refresh();
        assert!(
            matches!(got, Err(ReadError::Log(LogError::Unanchored(_)))),
            "{got:?}"
        );
    }

    #[test]
    fn an_edit_in_place_that_keeps_the_size_and_the_time_is_still_read() {
        let dir = tempfile::tempdir().unwrap();
        let key = ed25519_dalek::SigningKey::from_bytes(&[4; 32]);
        let public = key.verifying_key();
        let lock = log::Lock::take(dir.path()).unwrap();
        log::save_anchor(&lock, None, "c0").unwrap();
        let mut store = Store::open(lock, key, None).unwrap();
        observe(&mut store, 1);
        let mut reader = Reader::open(dir.path(), public).unwrap();
        let log = log::log_path(dir.path());
        let was = fs::metadata(&log).unwrap().modified().unwrap();
        let text = fs::read_to_string(&log).unwrap();
        let mut file = fs::File::options().write(true).open(&log).unwrap();
        std::io::Write::write_all(&mut file, text.replace("someone", "somebdy").as_bytes())
            .unwrap();
        file.set_modified(was).unwrap();
        drop(file);
        assert_eq!(fs::metadata(&log).unwrap().modified().unwrap(), was);
        assert!(reader.refresh().is_err());
    }

    #[test]
    fn a_log_that_stops_verifying_fails_the_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let key = ed25519_dalek::SigningKey::from_bytes(&[4; 32]);
        let public = key.verifying_key();
        let lock = log::Lock::take(dir.path()).unwrap();
        log::save_anchor(&lock, None, "c0").unwrap();
        let mut store = Store::open(lock, key, None).unwrap();
        observe(&mut store, 1);
        let mut reader = Reader::open(dir.path(), public).unwrap();
        let log = log::log_path(dir.path());
        let text = fs::read_to_string(&log).unwrap();
        fs::write(&log, text.replace("someone", "somebody")).unwrap();
        assert!(reader.refresh().is_err());
    }
}
