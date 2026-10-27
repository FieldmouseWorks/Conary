//! The op log (`docs/DESIGN.md` sections 4.6 and 8): one append-only file,
//! one operation per line, each hash-chained to the one before it and signed
//! by the ledger's key. It's the ledger's source of truth; everything else is
//! rebuilt by replaying it.
//!
//! A line is `{"id":…,"sig":…,"op":{…}}`. The id is the hash of the `op`
//! bytes exactly as written, so verifying a line never depends on how a
//! later build would serialize it.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::Deserialize;
use serde_json::value::RawValue;

use crate::id::OpId;
use crate::op::{Body, Op, Unsigned};

const LOG_FILE: &str = "ops.jsonl";
const LOCK_FILE: &str = "lock";

#[derive(Debug)]
pub enum LogError {
    Io(io::Error),
    /// A line that doesn't verify. The log is refused whole: nothing is
    /// trusted past a line that's been tampered with.
    Corrupt {
        line: usize,
        reason: String,
    },
    /// The key file isn't a 32-byte ed25519 secret key.
    Key(PathBuf),
    /// The log doesn't hold the op its caller anchored it at: whole lines
    /// were cut from its end, or it was removed or replaced.
    Unanchored(Anchor),
    /// An earlier append failed partway, so the file may not end where this
    /// writer thinks it does. Reopening reconciles it.
    Poisoned,
}

impl std::fmt::Display for LogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "op log: {e}"),
            Self::Corrupt { line, reason } => write!(f, "op log line {line}: {reason}"),
            Self::Key(path) => write!(f, "{}: not an ed25519 secret key", path.display()),
            Self::Unanchored(a) => write!(f, "op log: op {} isn't op {} of the log", a.op, a.seq),
            Self::Poisoned => f.write_str("op log: an earlier append failed; reopen the log"),
        }
    }
}

impl std::error::Error for LogError {}

impl From<io::Error> for LogError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// The directory a path is in.
fn parent_of(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

/// Makes a directory's entries (a new file, a rename, a link) survive power
/// loss: syncing a file doesn't sync the directory that names it.
fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(test)]
    tests::synced(dir)?;
    File::open(dir)?.sync_all()
}

/// Creates `dir` and any missing ancestors, and syncs every entry on the way
/// down to it into the directory that holds it. That includes entries that
/// already exist: another caller may have made one and not synced it yet, or
/// failed to, and seeing it there doesn't make it durable.
fn publish_dir(dir: &Path) -> io::Result<()> {
    let Some(parent) = dir.parent() else {
        return Ok(()); // The root, which nothing names.
    };
    if !parent.as_os_str().is_empty() {
        publish_dir(parent)?;
    }
    match fs::create_dir(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && dir.is_dir() => {}
        Err(e) => return Err(e),
    }
    // No exceptions, an ancestor this user can write to but not open
    // included: an entry made there can't be synced, so it isn't durable.
    sync_dir(parent_of(dir))
}

fn load_key(path: &Path, bytes: Vec<u8>) -> Result<SigningKey, LogError> {
    let secret: [u8; 32] = bytes
        .try_into()
        .map_err(|_| LogError::Key(path.to_owned()))?;
    Ok(SigningKey::from_bytes(&secret))
}

/// The ledger's signing key: loaded from `path`, or made there (readable by
/// its owner only) the first time. Either way it's on disk, and named there,
/// before it's returned.
pub fn load_or_create_key(path: &Path) -> Result<SigningKey, LogError> {
    let dir = parent_of(path);
    publish_dir(dir)?;
    let key = match fs::read(path) {
        Ok(bytes) => load_key(path, bytes)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => create_key(path, dir)?,
        Err(e) => return Err(e.into()),
    };
    // Whoever made it, this caller or one racing it, may not have finished
    // syncing it.
    File::open(path)?.sync_all()?;
    sync_dir(dir)?;
    Ok(key)
}

fn create_key(path: &Path, dir: &Path) -> Result<SigningKey, LogError> {
    let mut secret = [0u8; 32];
    let mut nonce = [0u8; 8];
    getrandom::fill(&mut secret).map_err(|e| LogError::Io(io::Error::other(e)))?;
    getrandom::fill(&mut nonce).map_err(|e| LogError::Io(io::Error::other(e)))?;
    // Written whole under a name of its own, then linked into place, so the
    // key's path never holds part of a key. When two callers race, the first
    // link wins and the other loads it.
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = dir.join(format!(".{name}.{}.new", hex(&nonce)));
    let written = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(&secret)?;
        file.sync_all()?;
        fs::hard_link(&tmp, path)
    })();
    let _ = fs::remove_file(&tmp);
    match written {
        Ok(()) => Ok(SigningKey::from_bytes(&secret)),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => load_key(path, fs::read(path)?),
        Err(e) => Err(e.into()),
    }
}

/// A point in the log that its caller knows was written: the `seq`th op
/// (from 1) is `op`. Hash chains can't show that whole lines were cut from
/// the end, so the caller keeps an anchor somewhere the log's own writer
/// can't rewrite (the GitHub notes, `docs/DESIGN.md` section 4.6), and a
/// log that doesn't hold it is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Anchor {
    pub seq: usize,
    pub op: OpId,
}

impl Anchor {
    fn check(self, ops: &[Op]) -> Result<(), LogError> {
        match self.seq.checked_sub(1).and_then(|i| ops.get(i)) {
            Some(op) if op.id == self.op => Ok(()),
            _ => Err(LogError::Unanchored(self)),
        }
    }
}

#[derive(Deserialize)]
struct Line<'a> {
    id: OpId,
    sig: &'a str,
    #[serde(borrow)]
    op: &'a RawValue,
}

/// How the log's bytes end.
#[derive(Debug, PartialEq, Eq)]
enum Tail {
    /// On a newline.
    Whole,
    /// On a complete op with no newline after it. It's kept: its bytes are
    /// all there and they verify.
    Unterminated,
    /// On part of a line from `at`: a write that never finished, whose caller
    /// was never told it succeeded, so it's left out. Only the start of a
    /// line, cut short, counts: bytes that could never become a whole line
    /// that verifies are refused like any other bad line.
    Torn { at: usize },
}

/// Verifies one line, the `n`th, as the op after `ops`.
fn verify_line(n: usize, raw: &[u8], ops: &[Op], key: &VerifyingKey) -> Result<Op, LogError> {
    let corrupt = |reason: &str| LogError::Corrupt {
        line: n,
        reason: reason.to_owned(),
    };
    let raw = std::str::from_utf8(raw).map_err(|_| corrupt("not UTF-8"))?;
    let line: Line = serde_json::from_str(raw).map_err(|e| corrupt(&e.to_string()))?;
    if OpId::of(line.op.get().as_bytes()) != line.id {
        return Err(corrupt("id doesn't match the op"));
    }
    let sig = parse_sig(line.sig).ok_or_else(|| corrupt("signature isn't 64 hex bytes"))?;
    key.verify_strict(line.id.as_bytes(), &sig)
        .map_err(|_| corrupt("signature doesn't verify against the ledger's key"))?;
    let op: Unsigned = serde_json::from_str(line.op.get()).map_err(|e| corrupt(&e.to_string()))?;
    if op.parent != ops.last().map(|o| o.id) {
        return Err(corrupt("parent isn't the op before it"));
    }
    Ok(Op { id: line.id, op })
}

/// Every op in the log's bytes, verified against `key` and its chain.
fn verify(bytes: &[u8], key: &VerifyingKey) -> Result<(Vec<Op>, Tail), LogError> {
    let end = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    let (whole, rest) = bytes.split_at(end);
    let mut ops: Vec<Op> = Vec::new();
    let lines = whole.strip_suffix(b"\n").map(|w| w.split(|&b| b == b'\n'));
    for (i, raw) in lines.into_iter().flatten().enumerate() {
        let op = verify_line(i + 1, raw, &ops, key)?;
        ops.push(op);
    }
    if rest.is_empty() {
        return Ok((ops, Tail::Whole));
    }
    if is_cut_short(rest) {
        return Ok((ops, Tail::Torn { at: end }));
    }
    // Complete, so it verifies like any other line or the log is refused.
    let op = verify_line(ops.len() + 1, rest, &ops, key)?;
    ops.push(op);
    Ok((ops, Tail::Unterminated))
}

/// Whether `rest` is the start of a line that ends before its JSON does,
/// a character split at the end included: what a torn write leaves.
fn is_cut_short(rest: &[u8]) -> bool {
    let text = match std::str::from_utf8(rest) {
        Ok(text) => text,
        Err(e) if e.error_len().is_none() => {
            std::str::from_utf8(&rest[..e.valid_up_to()]).expect("valid up to here")
        }
        Err(_) => return false,
    };
    matches!(
        serde_json::from_str::<Line>(text),
        Err(e) if e.classify() == serde_json::error::Category::Eof
    )
}

fn parse_sig(text: &str) -> Option<Signature> {
    if text.len() != 128 {
        return None;
    }
    let mut bytes = [0u8; 64];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(Signature::from_bytes(&bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Every op in the log at `dir`, verified against the ledger's public key
/// and, when there is one, the newest anchor its caller holds. A reader
/// takes no lock: a write still in progress is only a torn last line, which
/// it leaves out.
pub fn read(dir: &Path, key: &VerifyingKey, anchor: Option<Anchor>) -> Result<Vec<Op>, LogError> {
    let ops = match fs::read(dir.join(LOG_FILE)) {
        Ok(bytes) => verify(&bytes, key)?.0,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.into()),
    };
    anchor.map_or(Ok(()), |a| a.check(&ops))?;
    Ok(ops)
}

/// The log, open for appending. Only one writer at a time: opening takes an
/// exclusive lock on the directory's lock file and holds it until dropped.
pub struct OpLog {
    file: File,
    _lock: File,
    key: SigningKey,
    head: Option<Anchor>,
    poisoned: bool,
    #[cfg(test)]
    fail_next_write: bool,
}

impl OpLog {
    /// Opens (or starts) the log at `dir`, verifies every op in it against
    /// `key` and, when there is one, the newest anchor its caller holds, and
    /// returns them with the log ready to append after them. Nothing in the
    /// file changes until it has verified.
    pub fn open(
        dir: &Path,
        key: SigningKey,
        anchor: Option<Anchor>,
    ) -> Result<(Self, Vec<Op>), LogError> {
        publish_dir(dir)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(LOCK_FILE))?;
        lock.lock()?;
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(dir.join(LOG_FILE))?;
        // The log and its entry, before any op in it is acknowledged: a
        // writer before this one may have failed to sync either.
        file.sync_all()?;
        sync_dir(dir)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let (ops, tail) = verify(&bytes, &key.verifying_key())?;
        anchor.map_or(Ok(()), |a| a.check(&ops))?;
        match tail {
            Tail::Whole => {}
            Tail::Unterminated => {
                file.write_all(b"\n")?;
                file.sync_data()?;
            }
            Tail::Torn { at } => {
                // Never a recorded op: its append didn't finish, so nobody
                // was told it happened. Cutting it keeps the next line whole.
                file.set_len(at as u64)?;
                file.sync_all()?;
            }
        }
        file.seek(SeekFrom::End(0))?;
        let head = ops.last().map(|o| Anchor {
            seq: ops.len(),
            op: o.id,
        });
        Ok((
            Self {
                file,
                _lock: lock,
                key,
                head,
                poisoned: false,
                #[cfg(test)]
                fail_next_write: false,
            },
            ops,
        ))
    }

    pub fn verifying_key(&self) -> VerifyingKey {
        self.key.verifying_key()
    }

    /// The last op written, to anchor the log at.
    pub fn head(&self) -> Option<Anchor> {
        self.head
    }

    /// Appends one op after the last, signed, and returns it once it's on
    /// disk.
    pub fn append(&mut self, body: Body, at: i64) -> Result<Op, LogError> {
        if self.poisoned {
            return Err(LogError::Poisoned);
        }
        let op = Unsigned {
            parent: self.head.map(|h| h.op),
            at,
            body,
        };
        let json = serde_json::to_string(&op).map_err(|e| LogError::Io(io::Error::other(e)))?;
        let id = OpId::of(json.as_bytes());
        let sig = self.key.sign(id.as_bytes());
        let line = format!(
            "{{\"id\":\"{id}\",\"sig\":\"{}\",\"op\":{json}}}\n",
            hex(&sig.to_bytes())
        );
        if let Err(e) = self.write(line.as_bytes()) {
            // The line may be on disk whole, in part or not at all, and only
            // reopening, which verifies the file again, can tell which.
            self.poisoned = true;
            return Err(e.into());
        }
        self.head = Some(Anchor {
            seq: self.head.map_or(1, |h| h.seq + 1),
            op: id,
        });
        Ok(Op { id, op })
    }

    fn write(&mut self, line: &[u8]) -> io::Result<()> {
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next_write) {
            // Half the line reaches the file, then the write fails.
            self.file.write_all(&line[..line.len() / 2])?;
            return Err(io::Error::other("injected"));
        }
        self.file.write_all(line)?;
        self.file.sync_data()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::ChangeId;
    use std::cell::RefCell;

    thread_local! {
        /// Every directory this thread synced, in order.
        static SYNCED: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
        /// A directory whose next sync on this thread fails.
        static FAIL_SYNC: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    }

    pub(super) fn synced(dir: &Path) -> io::Result<()> {
        if FAIL_SYNC.with_borrow(|f| f.as_deref() == Some(dir)) {
            FAIL_SYNC.set(None);
            return Err(io::Error::other("injected"));
        }
        SYNCED.with_borrow_mut(|s| s.push(dir.to_owned()));
        Ok(())
    }

    fn take_synced() -> Vec<PathBuf> {
        SYNCED.take()
    }

    /// Asserts `dir`'s entry, and every ancestor's, was synced into the
    /// directory holding it, and `dir` itself for what's in it.
    fn assert_published(synced: &[PathBuf], dir: &Path) {
        for d in dir.ancestors().skip(1).chain([dir]) {
            assert!(synced.iter().any(|s| s == d), "{} not synced", d.display());
        }
    }

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[7; 32])
    }

    fn close() -> Body {
        Body::Close {
            change: ChangeId::new().unwrap(),
        }
    }

    #[test]
    fn appended_ops_replay_in_order_and_chain() {
        let dir = tempfile::tempdir().unwrap();
        let (mut log, ops) = OpLog::open(dir.path(), key(), None).unwrap();
        assert!(ops.is_empty());
        let a = log.append(close(), 1).unwrap();
        let b = log.append(close(), 2).unwrap();
        drop(log);
        let ops = read(dir.path(), &key().verifying_key(), None).unwrap();
        assert_eq!(ops, vec![a.clone(), b.clone()]);
        assert_eq!(ops[1].op.parent, Some(a.id));
        let (_, reopened) = OpLog::open(dir.path(), key(), None).unwrap();
        assert_eq!(reopened.len(), 2);
    }

    fn tamper(dir: &Path, edit: impl Fn(String) -> String) -> LogError {
        let path = dir.join(LOG_FILE);
        fs::write(&path, edit(fs::read_to_string(&path).unwrap())).unwrap();
        read(dir, &key().verifying_key(), None).unwrap_err()
    }

    fn two_ops() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let (mut log, _) = OpLog::open(dir.path(), key(), None).unwrap();
        log.append(close(), 1).unwrap();
        log.append(close(), 2).unwrap();
        dir
    }

    #[test]
    fn an_edited_op_is_refused() {
        let dir = two_ops();
        let err = tamper(dir.path(), |t| t.replacen("\"at\":1", "\"at\":9", 1));
        assert!(matches!(err, LogError::Corrupt { line: 1, .. }), "{err}");
    }

    #[test]
    fn a_deleted_op_is_refused() {
        let dir = two_ops();
        let err = tamper(dir.path(), |t| t.lines().nth(1).unwrap().to_owned() + "\n");
        assert!(matches!(err, LogError::Corrupt { line: 1, .. }), "{err}");
    }

    #[test]
    fn reordered_ops_are_refused() {
        let dir = two_ops();
        let err = tamper(dir.path(), |t| {
            let lines: Vec<&str> = t.lines().collect();
            format!("{}\n{}\n", lines[1], lines[0])
        });
        assert!(matches!(err, LogError::Corrupt { line: 1, .. }), "{err}");
    }

    #[test]
    fn a_log_signed_by_another_key_is_refused() {
        let dir = two_ops();
        let other = SigningKey::from_bytes(&[8; 32]).verifying_key();
        let err = read(dir.path(), &other, None).unwrap_err();
        assert!(matches!(err, LogError::Corrupt { line: 1, .. }), "{err}");
    }

    #[test]
    fn a_torn_last_line_is_left_out_then_cut_by_the_writer() {
        let dir = two_ops();
        let path = dir.path().join(LOG_FILE);
        let whole = fs::read_to_string(&path).unwrap();
        fs::write(&path, format!("{whole}{{\"id\":\"ab")).unwrap();
        assert_eq!(
            read(dir.path(), &key().verifying_key(), None)
                .unwrap()
                .len(),
            2
        );
        let (mut log, ops) = OpLog::open(dir.path(), key(), None).unwrap();
        assert_eq!(ops.len(), 2);
        log.append(close(), 3).unwrap();
        drop(log);
        assert_eq!(
            read(dir.path(), &key().verifying_key(), None)
                .unwrap()
                .len(),
            3
        );
    }

    fn intent(summary: &str) -> Body {
        Body::Intent {
            change: ChangeId::new().unwrap(),
            summary: summary.into(),
            body: String::new(),
            task: None,
        }
    }

    #[test]
    fn a_write_torn_inside_a_character_is_left_out_then_cut() {
        let dir = two_ops();
        let path = dir.path().join(LOG_FILE);
        let whole = fs::read(&path).unwrap();
        let (mut log, _) = OpLog::open(dir.path(), key(), None).unwrap();
        log.append(intent("café"), 3).unwrap();
        drop(log);
        let full = fs::read(&path).unwrap();
        let e = full[whole.len()..]
            .windows(2)
            .position(|w| w == "é".as_bytes())
            .unwrap();
        // A line cut halfway through the two bytes of "é".
        let cut = whole.len() + e + 1;
        assert!(std::str::from_utf8(&full[..cut]).is_err());
        fs::write(&path, &full[..cut]).unwrap();
        assert_eq!(
            read(dir.path(), &key().verifying_key(), None)
                .unwrap()
                .len(),
            2
        );
        let (mut log, ops) = OpLog::open(dir.path(), key(), None).unwrap();
        assert_eq!(ops.len(), 2);
        log.append(close(), 4).unwrap();
        drop(log);
        assert_eq!(
            read(dir.path(), &key().verifying_key(), None)
                .unwrap()
                .len(),
            3
        );

        // A torn first op, too.
        fs::write(&path, &whole[..10]).unwrap();
        assert!(
            read(dir.path(), &key().verifying_key(), None)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_complete_line_that_isnt_utf8_is_refused() {
        let dir = two_ops();
        let path = dir.path().join(LOG_FILE);
        let mut bytes = fs::read(&path).unwrap();
        bytes[3] = 0xff;
        fs::write(&path, bytes).unwrap();
        let err = read(dir.path(), &key().verifying_key(), None).unwrap_err();
        assert!(matches!(err, LogError::Corrupt { line: 1, .. }), "{err}");
    }

    #[test]
    fn a_last_op_missing_only_its_newline_is_kept() {
        let dir = two_ops();
        let path = dir.path().join(LOG_FILE);
        let whole = fs::read_to_string(&path).unwrap();
        fs::write(&path, whole.strip_suffix('\n').unwrap()).unwrap();
        assert_eq!(
            read(dir.path(), &key().verifying_key(), None)
                .unwrap()
                .len(),
            2
        );
        let (mut log, ops) = OpLog::open(dir.path(), key(), None).unwrap();
        assert_eq!(ops.len(), 2);
        log.append(close(), 3).unwrap();
        drop(log);
        assert_eq!(
            read(dir.path(), &key().verifying_key(), None)
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn a_failed_append_stops_the_writer_until_the_log_is_reopened() {
        let dir = two_ops();
        let (mut log, _) = OpLog::open(dir.path(), key(), None).unwrap();
        log.fail_next_write = true;
        assert!(matches!(log.append(close(), 3), Err(LogError::Io(_))));
        assert!(matches!(log.append(close(), 4), Err(LogError::Poisoned)));
        drop(log);
        // Reopening cuts the half line, and the next op chains onto the last
        // one that was written whole.
        let (mut log, ops) = OpLog::open(dir.path(), key(), None).unwrap();
        assert_eq!(ops.len(), 2);
        log.append(close(), 5).unwrap();
        drop(log);
        let ops = read(dir.path(), &key().verifying_key(), None).unwrap();
        assert_eq!(ops.len(), 3);
        assert_eq!(ops[2].op.at, 5);
    }

    #[test]
    fn racing_first_users_all_get_the_one_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deep/er/key");
        let keys: Vec<[u8; 32]> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    s.spawn(|| {
                        let key = load_or_create_key(&path).unwrap().to_bytes();
                        // Every caller, winner or not, saw the key's entry synced.
                        assert_published(&take_synced(), path.parent().unwrap());
                        key
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert!(keys.iter().all(|k| *k == keys[0]));
        assert_eq!(load_or_create_key(&path).unwrap().to_bytes(), keys[0]);
        // No half-made key is left lying around.
        let names: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["key"]);
    }

    #[test]
    fn a_key_is_made_once_then_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key");
        let made = load_or_create_key(&path).unwrap();
        assert_eq!(
            load_or_create_key(&path).unwrap().to_bytes(),
            made.to_bytes()
        );
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    /// Asserts the log at `dir` is refused, by readers and the writer
    /// alike, and that refusing it changed nothing in it.
    fn assert_refused(dir: &Path, anchor: Option<Anchor>) -> LogError {
        let path = dir.join(LOG_FILE);
        let before = fs::read(&path).ok();
        let read_err = read(dir, &key().verifying_key(), anchor).unwrap_err();
        let Err(open_err) = OpLog::open(dir, key(), anchor) else {
            panic!("the writer opened a log the reader refused: {read_err}");
        };
        assert_eq!(read_err.to_string(), open_err.to_string());
        if let Some(before) = before {
            assert_eq!(fs::read(&path).unwrap(), before, "refusing changed the log");
        }
        read_err
    }

    fn unterminate(dir: &Path) {
        let path = dir.join(LOG_FILE);
        let whole = fs::read_to_string(&path).unwrap();
        fs::write(&path, whole.strip_suffix('\n').unwrap()).unwrap();
    }

    #[test]
    fn an_unterminated_first_op_under_another_key_is_refused_not_cut() {
        let dir = tempfile::tempdir().unwrap();
        let (mut log, _) = OpLog::open(dir.path(), SigningKey::from_bytes(&[8; 32]), None).unwrap();
        log.append(close(), 1).unwrap();
        drop(log);
        unterminate(dir.path());
        let err = assert_refused(dir.path(), None);
        assert!(matches!(err, LogError::Corrupt { line: 1, .. }), "{err}");
    }

    #[test]
    fn an_unterminated_last_op_that_doesnt_verify_is_refused_not_cut() {
        type Edit = fn(&str, &str) -> String;
        let edits: [(&str, Edit); 4] = [
            ("id", |first, last| {
                format!("{first}\n{}", last.replacen("\"at\":2", "\"at\":9", 1))
            }),
            ("signature", |first, last| {
                let at = last.find("\"sig\":\"").unwrap() + 7;
                let flip = if &last[at..at + 1] == "0" { "1" } else { "0" };
                format!("{first}\n{}{flip}{}", &last[..at], &last[at + 1..])
            }),
            ("parent", |first, _| format!("{first}\n{first}")),
            ("not JSON at all", |first, _| format!("{first}\nnot a line")),
        ];
        for (what, edit) in edits {
            let dir = two_ops();
            let path = dir.path().join(LOG_FILE);
            let text = fs::read_to_string(&path).unwrap();
            let lines: Vec<&str> = text.lines().collect();
            fs::write(&path, edit(lines[0], lines[1])).unwrap();
            let err = assert_refused(dir.path(), None);
            assert!(
                matches!(err, LogError::Corrupt { line: 2, .. }),
                "{what}: {err}"
            );
        }
    }

    #[test]
    fn an_unfinished_tail_with_bytes_that_cant_be_utf8_is_refused() {
        let dir = two_ops();
        let path = dir.path().join(LOG_FILE);
        let mut bytes = fs::read(&path).unwrap();
        bytes.extend_from_slice(b"{\"id\":\"\xff\xfeab");
        fs::write(&path, bytes).unwrap();
        let err = assert_refused(dir.path(), None);
        assert!(matches!(err, LogError::Corrupt { line: 3, .. }), "{err}");
    }

    #[test]
    fn a_signed_op_of_an_unknown_schema_is_refused() {
        let json = r#"{"parent":null,"at":1,"body":{"kind":"unheard_of"}}"#;
        let id = OpId::of(json.as_bytes());
        let sig = hex(&key().sign(id.as_bytes()).to_bytes());
        let line = format!("{{\"id\":\"{id}\",\"sig\":\"{sig}\",\"op\":{json}}}");
        for ending in ["\n", ""] {
            let dir = tempfile::tempdir().unwrap();
            fs::write(dir.path().join(LOG_FILE), format!("{line}{ending}")).unwrap();
            let err = assert_refused(dir.path(), None);
            assert!(matches!(err, LogError::Corrupt { line: 1, .. }), "{err}");
        }
    }

    fn anchored_two_ops() -> (tempfile::TempDir, Anchor) {
        let dir = tempfile::tempdir().unwrap();
        let (mut log, _) = OpLog::open(dir.path(), key(), None).unwrap();
        assert_eq!(log.head(), None);
        let a = log.append(close(), 1).unwrap();
        assert_eq!(log.head(), Some(Anchor { seq: 1, op: a.id }));
        let b = log.append(close(), 2).unwrap();
        let head = log.head().unwrap();
        assert_eq!(head, Anchor { seq: 2, op: b.id });
        drop(log);
        // Reopening finds the same head.
        let (log, _) = OpLog::open(dir.path(), key(), Some(head)).unwrap();
        assert_eq!(log.head(), Some(head));
        (dir, head)
    }

    #[test]
    fn a_log_cut_short_by_whole_lines_is_refused_against_its_anchor() {
        let (dir, anchor) = anchored_two_ops();
        let path = dir.path().join(LOG_FILE);
        let text = fs::read_to_string(&path).unwrap();
        fs::write(&path, text.lines().next().unwrap().to_owned() + "\n").unwrap();
        let err = assert_refused(dir.path(), Some(anchor));
        assert!(
            matches!(err, LogError::Unanchored(a) if a == anchor),
            "{err}"
        );
        // Without the anchor it still chains: why the anchor lives off the box.
        assert_eq!(
            read(dir.path(), &key().verifying_key(), None)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn an_anchored_op_cut_partway_is_refused_not_trimmed() {
        let (dir, anchor) = anchored_two_ops();
        let path = dir.path().join(LOG_FILE);
        let bytes = fs::read(&path).unwrap();
        // It now looks like a torn write, but it was acknowledged.
        fs::write(&path, &bytes[..bytes.len() - 10]).unwrap();
        let err = assert_refused(dir.path(), Some(anchor));
        assert!(matches!(err, LogError::Unanchored(_)), "{err}");
    }

    #[test]
    fn a_missing_log_is_refused_against_its_anchor() {
        let (dir, anchor) = anchored_two_ops();
        fs::remove_file(dir.path().join(LOG_FILE)).unwrap();
        let err = assert_refused(dir.path(), Some(anchor));
        assert!(matches!(err, LogError::Unanchored(_)), "{err}");
        let err = read(
            &dir.path().join("elsewhere"),
            &key().verifying_key(),
            Some(anchor),
        );
        assert!(matches!(err, Err(LogError::Unanchored(_))));
    }

    #[test]
    fn a_replaced_log_is_refused_against_its_anchor() {
        let (dir, anchor) = anchored_two_ops();
        // Another history, as long and signed by the same key.
        let other = two_ops();
        fs::copy(other.path().join(LOG_FILE), dir.path().join(LOG_FILE)).unwrap();
        let err = assert_refused(dir.path(), Some(anchor));
        assert!(matches!(err, LogError::Unanchored(_)), "{err}");
    }

    #[test]
    fn a_log_grown_past_its_anchor_opens_and_a_torn_tail_is_still_cut() {
        let (dir, anchor) = anchored_two_ops();
        let first = read(dir.path(), &key().verifying_key(), None).unwrap()[0].id;
        let earlier = Anchor { seq: 1, op: first };
        let path = dir.path().join(LOG_FILE);
        let whole = fs::read_to_string(&path).unwrap();
        fs::write(&path, format!("{whole}{{\"id\":\"ab")).unwrap();
        for a in [earlier, anchor] {
            assert_eq!(
                read(dir.path(), &key().verifying_key(), Some(a))
                    .unwrap()
                    .len(),
                2
            );
        }
        let (mut log, ops) = OpLog::open(dir.path(), key(), Some(anchor)).unwrap();
        assert_eq!(ops.len(), 2);
        log.append(close(), 3).unwrap();
        let head = log.head().unwrap();
        drop(log);
        assert_eq!(head.seq, 3);
        assert_eq!(
            read(dir.path(), &key().verifying_key(), Some(head))
                .unwrap()
                .len(),
            3
        );
        // An anchor past the end, or at op 0, is never held.
        for a in [Anchor { seq: 4, ..head }, Anchor { seq: 0, ..head }] {
            let err = read(dir.path(), &key().verifying_key(), Some(a));
            assert!(matches!(err, Err(LogError::Unanchored(_))));
        }
    }

    #[test]
    fn a_directory_someone_else_made_is_still_synced_into_its_parent() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("a/b");
        // Made by another initializer that hasn't synced anything yet.
        fs::create_dir_all(&dir).unwrap();
        take_synced();
        OpLog::open(&dir, key(), None).unwrap();
        assert_published(&take_synced(), &dir);
    }

    #[test]
    fn a_retry_after_a_failed_sync_completes_it() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("a/b");
        FAIL_SYNC.set(Some(root.path().join("a")));
        assert!(matches!(
            OpLog::open(&dir, key(), None),
            Err(LogError::Io(_))
        ));
        // The directory was made, but its entry in `a` was never synced.
        assert!(dir.is_dir());
        take_synced();
        OpLog::open(&dir, key(), None).unwrap();
        assert_published(&take_synced(), &dir);

        let path = root.path().join("keys/key");
        FAIL_SYNC.set(Some(root.path().join("keys")));
        assert!(matches!(load_or_create_key(&path), Err(LogError::Io(_))));
        // The key was linked into place, but its entry was never synced.
        assert!(path.exists());
        take_synced();
        load_or_create_key(&path).unwrap();
        assert_published(&take_synced(), path.parent().unwrap());
    }

    #[test]
    fn a_key_someone_else_made_is_synced_before_it_is_loaded() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("deep/er/key");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, [9u8; 32]).unwrap();
        take_synced();
        assert_eq!(load_or_create_key(&path).unwrap().to_bytes(), [9u8; 32]);
        assert_published(&take_synced(), path.parent().unwrap());
    }

    #[test]
    fn a_key_and_a_log_in_different_directories_are_both_published() {
        let root = tempfile::tempdir().unwrap();
        let key_path = root.path().join("etc/conary/key");
        let log_dir = root.path().join("var/lib/conary");
        take_synced();
        let key = load_or_create_key(&key_path).unwrap();
        let (mut log, _) = OpLog::open(&log_dir, key, None).unwrap();
        log.append(close(), 1).unwrap();
        let synced = take_synced();
        assert_published(&synced, key_path.parent().unwrap());
        assert_published(&synced, &log_dir);
    }

    #[test]
    fn an_ancestor_that_cant_be_opened_to_sync_is_an_error_every_time() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let blind = root.path().join("blind");
        fs::create_dir(&blind).unwrap();
        // Writable and searchable, but not readable: entries can be made in
        // it, and it can't be opened to sync them.
        fs::set_permissions(&blind, fs::Permissions::from_mode(0o300)).unwrap();
        if File::open(&blind).is_ok() {
            return; // Run as root, which permissions don't stop.
        }
        let dir = blind.join("ledger");
        let key_path = blind.join("keys/key");
        for _ in 0..2 {
            // The retry finds the directory made, and still can't sync it.
            let err = OpLog::open(&dir, key(), None).err().unwrap();
            assert!(matches!(&err, LogError::Io(e) if e.kind() == io::ErrorKind::PermissionDenied));
            let err = load_or_create_key(&key_path).err().unwrap();
            assert!(matches!(&err, LogError::Io(e) if e.kind() == io::ErrorKind::PermissionDenied));
        }
        assert!(dir.is_dir());
        fs::set_permissions(&blind, fs::Permissions::from_mode(0o700)).unwrap();
        OpLog::open(&dir, key(), None).unwrap();
        load_or_create_key(&key_path).unwrap();
    }
}
