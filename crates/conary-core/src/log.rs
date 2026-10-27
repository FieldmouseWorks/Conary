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
    File::open(dir)?.sync_all()
}

/// Creates `dir` and any missing ancestors, syncing each new entry into the
/// directory that holds it.
fn create_dir_durably(dir: &Path) -> io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    let parent = parent_of(dir);
    if parent != dir {
        create_dir_durably(parent)?;
    }
    match fs::create_dir(dir) {
        Ok(()) => sync_dir(parent),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && dir.is_dir() => Ok(()),
        Err(e) => Err(e),
    }
}

fn load_key(path: &Path, bytes: Vec<u8>) -> Result<SigningKey, LogError> {
    let secret: [u8; 32] = bytes
        .try_into()
        .map_err(|_| LogError::Key(path.to_owned()))?;
    Ok(SigningKey::from_bytes(&secret))
}

/// The ledger's signing key: loaded from `path`, or made there (readable by
/// its owner only) the first time.
pub fn load_or_create_key(path: &Path) -> Result<SigningKey, LogError> {
    match fs::read(path) {
        Ok(bytes) => return load_key(path, bytes),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let dir = parent_of(path);
    create_dir_durably(dir)?;
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
        Ok(()) => {
            sync_dir(dir)?;
            Ok(SigningKey::from_bytes(&secret))
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => load_key(path, fs::read(path)?),
        Err(e) => Err(e.into()),
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
    /// was never told it succeeded, so it's left out.
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
    // Whatever bytes a torn write left, split characters included.
    match verify_line(ops.len() + 1, rest, &ops, key) {
        Ok(op) => {
            ops.push(op);
            Ok((ops, Tail::Unterminated))
        }
        Err(_) => Ok((ops, Tail::Torn { at: end })),
    }
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

/// Every op in the log at `dir`, verified against the ledger's public key.
/// A reader takes no lock: a write still in progress is only a torn last
/// line, which it leaves out.
pub fn read(dir: &Path, key: &VerifyingKey) -> Result<Vec<Op>, LogError> {
    match fs::read(dir.join(LOG_FILE)) {
        Ok(bytes) => Ok(verify(&bytes, key)?.0),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

/// The log, open for appending. Only one writer at a time: opening takes an
/// exclusive lock on the directory's lock file and holds it until dropped.
pub struct OpLog {
    file: File,
    _lock: File,
    key: SigningKey,
    head: Option<OpId>,
    poisoned: bool,
    #[cfg(test)]
    fail_next_write: bool,
}

impl OpLog {
    /// Opens (or starts) the log at `dir`, verifies every op in it against
    /// `key`, and returns them with the log ready to append after them.
    pub fn open(dir: &Path, key: SigningKey) -> Result<(Self, Vec<Op>), LogError> {
        create_dir_durably(dir)?;
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
        // The log's entry, if it's new, before any op in it is acknowledged.
        sync_dir(dir)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let (ops, tail) = verify(&bytes, &key.verifying_key())?;
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
        let head = ops.last().map(|o| o.id);
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

    /// Appends one op after the last, signed, and returns it once it's on
    /// disk.
    pub fn append(&mut self, body: Body, at: i64) -> Result<Op, LogError> {
        if self.poisoned {
            return Err(LogError::Poisoned);
        }
        let op = Unsigned {
            parent: self.head,
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
        self.head = Some(id);
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
        let (mut log, ops) = OpLog::open(dir.path(), key()).unwrap();
        assert!(ops.is_empty());
        let a = log.append(close(), 1).unwrap();
        let b = log.append(close(), 2).unwrap();
        drop(log);
        let ops = read(dir.path(), &key().verifying_key()).unwrap();
        assert_eq!(ops, vec![a.clone(), b.clone()]);
        assert_eq!(ops[1].op.parent, Some(a.id));
        let (_, reopened) = OpLog::open(dir.path(), key()).unwrap();
        assert_eq!(reopened.len(), 2);
    }

    fn tamper(dir: &Path, edit: impl Fn(String) -> String) -> LogError {
        let path = dir.join(LOG_FILE);
        fs::write(&path, edit(fs::read_to_string(&path).unwrap())).unwrap();
        read(dir, &key().verifying_key()).unwrap_err()
    }

    fn two_ops() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let (mut log, _) = OpLog::open(dir.path(), key()).unwrap();
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
        let err = read(dir.path(), &other).unwrap_err();
        assert!(matches!(err, LogError::Corrupt { line: 1, .. }), "{err}");
    }

    #[test]
    fn a_torn_last_line_is_left_out_then_cut_by_the_writer() {
        let dir = two_ops();
        let path = dir.path().join(LOG_FILE);
        let whole = fs::read_to_string(&path).unwrap();
        fs::write(&path, format!("{whole}{{\"id\":\"ab")).unwrap();
        assert_eq!(read(dir.path(), &key().verifying_key()).unwrap().len(), 2);
        let (mut log, ops) = OpLog::open(dir.path(), key()).unwrap();
        assert_eq!(ops.len(), 2);
        log.append(close(), 3).unwrap();
        drop(log);
        assert_eq!(read(dir.path(), &key().verifying_key()).unwrap().len(), 3);
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
        let (mut log, _) = OpLog::open(dir.path(), key()).unwrap();
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
        assert_eq!(read(dir.path(), &key().verifying_key()).unwrap().len(), 2);
        let (mut log, ops) = OpLog::open(dir.path(), key()).unwrap();
        assert_eq!(ops.len(), 2);
        log.append(close(), 4).unwrap();
        drop(log);
        assert_eq!(read(dir.path(), &key().verifying_key()).unwrap().len(), 3);

        // A torn first op, too.
        fs::write(&path, &whole[..10]).unwrap();
        assert!(read(dir.path(), &key().verifying_key()).unwrap().is_empty());
    }

    #[test]
    fn a_complete_line_that_isnt_utf8_is_refused() {
        let dir = two_ops();
        let path = dir.path().join(LOG_FILE);
        let mut bytes = fs::read(&path).unwrap();
        bytes[3] = 0xff;
        fs::write(&path, bytes).unwrap();
        let err = read(dir.path(), &key().verifying_key()).unwrap_err();
        assert!(matches!(err, LogError::Corrupt { line: 1, .. }), "{err}");
    }

    #[test]
    fn a_last_op_missing_only_its_newline_is_kept() {
        let dir = two_ops();
        let path = dir.path().join(LOG_FILE);
        let whole = fs::read_to_string(&path).unwrap();
        fs::write(&path, whole.strip_suffix('\n').unwrap()).unwrap();
        assert_eq!(read(dir.path(), &key().verifying_key()).unwrap().len(), 2);
        let (mut log, ops) = OpLog::open(dir.path(), key()).unwrap();
        assert_eq!(ops.len(), 2);
        log.append(close(), 3).unwrap();
        drop(log);
        assert_eq!(read(dir.path(), &key().verifying_key()).unwrap().len(), 3);
    }

    #[test]
    fn a_failed_append_stops_the_writer_until_the_log_is_reopened() {
        let dir = two_ops();
        let (mut log, _) = OpLog::open(dir.path(), key()).unwrap();
        log.fail_next_write = true;
        assert!(matches!(log.append(close(), 3), Err(LogError::Io(_))));
        assert!(matches!(log.append(close(), 4), Err(LogError::Poisoned)));
        drop(log);
        // Reopening cuts the half line, and the next op chains onto the last
        // one that was written whole.
        let (mut log, ops) = OpLog::open(dir.path(), key()).unwrap();
        assert_eq!(ops.len(), 2);
        log.append(close(), 5).unwrap();
        drop(log);
        let ops = read(dir.path(), &key().verifying_key()).unwrap();
        assert_eq!(ops.len(), 3);
        assert_eq!(ops[2].op.at, 5);
    }

    #[test]
    fn racing_first_users_all_get_the_one_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deep/er/key");
        let keys: Vec<[u8; 32]> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..8)
                .map(|_| s.spawn(|| load_or_create_key(&path).unwrap().to_bytes()))
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
}
