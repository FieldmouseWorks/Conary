//! Opening the op log for writing without verifying its prefix under the
//! writer lock.
//!
//! [`OpLog::prepare`] reads and verifies every complete line currently in the
//! log before the lock is taken, keeping the file's identity and a streaming
//! BLAKE3 state over those exact verified bytes. Once the caller holds the
//! [`Lock`], [`OpLog::open_prepared`] checks that the log is still the same
//! file and hasn't shrunk or been modified in place, then reads and verifies
//! only the bytes appended since. Anything else falls back to verifying from
//! byte 0 under the lock.
//!
//! [`Opening`] holds the lock before the mark is updated or any torn tail is
//! mended, so its caller can verify every anchor checkpoint against the
//! verified op IDs and save its anchor copy under the same lock before
//! [`Opening::finish`] commits and lets the lock go.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use ed25519_dalek::{SigningKey, VerifyingKey};

use crate::id::OpId;
use crate::op::Op;

use super::verified::{self, Mark};
use super::{Anchor, LOG_FILE, Lock, LogError, OpLog, Tail, mend, sync_dir, verify, verify_whole};

/// Identity of the log file when [`OpLog::prepare`] read it: `(dev, ino,
/// ctime, ctime_nsec)`. If the file under the lock has a different `(dev, ino)`,
/// shrunk below the prepared offset, or kept the same length with a moved
/// change time, the prepared prefix isn't reused.
type Stamp = (u64, u64, i64, i64);

fn file_stamp(meta: &std::fs::Metadata) -> Stamp {
    (meta.dev(), meta.ino(), meta.ctime(), meta.ctime_nsec())
}

/// Hashes `whole` once with BLAKE3, checking `mark` at `mark.len()` along the
/// way so the returned hasher covers all of `whole` and `marked` is how many
/// leading bytes `mark` vouches for under `key`.
fn hash_prefix(whole: &[u8], key: &VerifyingKey, mark: Option<&Mark>) -> (usize, blake3::Hasher) {
    let mut hasher = blake3::Hasher::new();
    if let Some(m) = mark
        && m.len() <= whole.len()
    {
        hasher.update(&whole[..m.len()]);
        let marked = if m.verify_hash(&hasher.finalize(), key) {
            m.len()
        } else {
            0
        };
        hasher.update(&whole[m.len()..]);
        return (marked, hasher);
    }
    hasher.update(whole);
    (0, hasher)
}

/// Complete lines of the log read and verified before taking the writer lock.
pub struct Prepared {
    key: VerifyingKey,
    stamp: Option<Stamp>,
    ids: Vec<OpId>,
    ops: Vec<Op>,
    /// Byte offset of the end of the last complete `\n`-terminated line verified.
    len: u64,
    /// Streaming BLAKE3 state over `0..len` (the exact bytes verified in `ops`).
    hasher: blake3::Hasher,
}

impl Prepared {
    /// Moves the prepared ops out for a caller (such as [`crate::Store`]) that
    /// replays them before taking the lock.
    pub(crate) fn take_ops(&mut self) -> Vec<Op> {
        std::mem::take(&mut self.ops)
    }
}

/// The log verified under its [`Lock`], before its verified mark is updated or
/// its tail is mended. Dropping it without calling [`Opening::finish`] leaves
/// the log and its mark untouched and lets the lock go.
pub struct Opening {
    lock: Lock,
    file: File,
    key: SigningKey,
    ids: Vec<OpId>,
    /// Ops to return from [`Opening::finish`]: every op when opening from byte
    /// 0 (or when [`Prepared::take_ops`] wasn't called), or only the tail ops
    /// appended since [`OpLog::prepare`] when the prefix ops were taken.
    ops: Vec<Op>,
    reused_prefix: bool,
    from: u64,
    tail_bytes: Vec<u8>,
    tail: Tail,
    whole_len: usize,
    hasher: blake3::Hasher,
}

impl Opening {
    /// The writer lock held while opening.
    pub fn lock(&self) -> &Lock {
        &self.lock
    }

    /// Whether the prepared prefix was reused (so a caller that replayed it
    /// before taking the lock need only apply the ops returned by
    /// [`Opening::finish`]).
    pub(crate) fn reused_prefix(&self) -> bool {
        self.reused_prefix
    }

    /// Whether the verified log holds `anchor`.
    pub fn check(&self, anchor: Anchor) -> Result<(), LogError> {
        anchor.found(
            anchor
                .seq
                .checked_sub(1)
                .and_then(|i| self.ids.get(i))
                .copied(),
        )
    }

    /// Updates the verified mark, mends any torn or unterminated tail, lets
    /// the writer lock go, and returns the open log with its ops.
    pub fn finish(mut self) -> Result<(OpLog, Vec<Op>), LogError> {
        let dir = self.lock.dir.clone();
        let current_mark = Mark::load(&dir);
        verified::keep_hash(
            &dir,
            self.whole_len,
            &self.hasher.finalize(),
            &self.key,
            current_mark,
        )?;
        let len = mend(&mut self.file, self.from, &self.tail_bytes, self.tail)?;
        drop(self.lock);
        Ok((
            OpLog {
                file: self.file,
                dir,
                key: self.key,
                ids: self.ids,
                len,
                held: None,
                poisoned: false,
                #[cfg(test)]
                fail_next_write: false,
            },
            self.ops,
        ))
    }
}

impl OpLog {
    /// Reads and verifies every complete line currently in the log at `dir`
    /// without taking the writer lock. Passing the result to
    /// [`OpLog::open_prepared`] once the lock is held then reads and verifies
    /// only what other writers appended since.
    pub fn prepare(dir: &Path, key: SigningKey) -> Result<Prepared, LogError> {
        let verifying = key.verifying_key();
        // Load the mark before reading the log: one written after could cover
        // bytes past what we read.
        let mark = Mark::load(dir);
        let mut file = match File::open(dir.join(LOG_FILE)) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(Prepared {
                    key: verifying,
                    stamp: None,
                    ids: Vec::new(),
                    ops: Vec::new(),
                    len: 0,
                    hasher: blake3::Hasher::new(),
                });
            }
            Err(e) => return Err(e.into()),
        };
        // Capture the stamp before reading so any later write (including an
        // in-place overwrite of the same length) moves `ctime` past it.
        let stamp = file_stamp(&file.metadata()?);
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        // Leave any trailing bytes after the last newline for the locked open:
        // a concurrent writer may still be finishing that line, or it may be a
        // torn/unterminated tail that only the lock holder can mend.
        let end = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
        let whole = &bytes[..end];
        let (marked, hasher) = hash_prefix(whole, &verifying, mark.as_ref());
        let ops = verify_whole(whole, &verifying, 0, None, marked)?;
        let ids = ops.iter().map(|o| o.id).collect();
        Ok(Prepared {
            key: verifying,
            stamp: Some(stamp),
            ids,
            ops,
            len: end as u64,
            hasher,
        })
    }

    /// Opens (or starts) the log in `lock`'s directory under `lock`, reusing
    /// `prepared` when the file is unchanged up to its prepared offset and
    /// verifying only what was appended since. Nothing on disk is changed
    /// until [`Opening::finish`].
    pub fn open_prepared(
        prepared: Option<Prepared>,
        lock: Lock,
        key: SigningKey,
    ) -> Result<Opening, LogError> {
        let dir = lock.dir.clone();
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(dir.join(LOG_FILE))?;
        // The log and its entry, before any op in it is acknowledged: a
        // writer before this one may have failed to sync either.
        file.sync_all()?;
        sync_dir(&dir)?;

        let meta = file.metadata()?;
        let size = meta.len();
        let verifying = key.verifying_key();

        if let Some(mut p) = prepared {
            let can_reuse = p.key == verifying
                && match p.stamp {
                    None => size == 0 && p.len == 0,
                    Some((dev, ino, ctime, ctime_nsec)) => {
                        meta.dev() == dev
                            && meta.ino() == ino
                            && (size > p.len
                                || (size == p.len
                                    && meta.ctime() == ctime
                                    && meta.ctime_nsec() == ctime_nsec))
                    }
                };
            if can_reuse {
                let mut tail_bytes = Vec::new();
                let (tail_ops, tail) = if size > p.len {
                    file.seek(SeekFrom::Start(p.len))?;
                    (&file).read_to_end(&mut tail_bytes)?;
                    verify(
                        &tail_bytes,
                        &verifying,
                        p.ids.len(),
                        p.ids.last().copied(),
                        None,
                    )?
                } else {
                    (Vec::new(), Tail::Whole)
                };
                let tail_whole = tail_bytes
                    .iter()
                    .rposition(|&b| b == b'\n')
                    .map_or(0, |i| i + 1);
                p.hasher.update(&tail_bytes[..tail_whole]);
                p.ids.extend(tail_ops.iter().map(|o| o.id));
                p.ops.extend(tail_ops);
                let whole_len = usize::try_from(p.len)
                    .ok()
                    .and_then(|n| n.checked_add(tail_whole))
                    .ok_or_else(|| LogError::Io(io::Error::other("op log length overflow")))?;
                return Ok(Opening {
                    lock,
                    file,
                    key,
                    ids: p.ids,
                    ops: p.ops,
                    reused_prefix: true,
                    from: p.len,
                    tail_bytes,
                    tail,
                    whole_len,
                    hasher: p.hasher,
                });
            }
        }

        let mark = Mark::load(&dir);
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let end = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
        let (whole, rest) = bytes.split_at(end);
        let (marked, hasher) = hash_prefix(whole, &verifying, mark.as_ref());
        let (ops, tail) = super::verify_split(whole, rest, &verifying, 0, None, marked)?;
        let ids = ops.iter().map(|o| o.id).collect();
        Ok(Opening {
            lock,
            file,
            key,
            ids,
            ops,
            reused_prefix: false,
            from: 0,
            tail_bytes: bytes,
            tail,
            whole_len: end,
            hasher,
        })
    }
}
