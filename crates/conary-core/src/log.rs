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
}

impl std::fmt::Display for LogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "op log: {e}"),
            Self::Corrupt { line, reason } => write!(f, "op log line {line}: {reason}"),
            Self::Key(path) => write!(f, "{}: not an ed25519 secret key", path.display()),
        }
    }
}

impl std::error::Error for LogError {}

impl From<io::Error> for LogError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// The ledger's signing key: loaded from `path`, or made there (readable by
/// its owner only) the first time.
pub fn load_or_create_key(path: &Path) -> Result<SigningKey, LogError> {
    match fs::read(path) {
        Ok(bytes) => {
            let secret: [u8; 32] = bytes
                .try_into()
                .map_err(|_| LogError::Key(path.to_owned()))?;
            Ok(SigningKey::from_bytes(&secret))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let mut secret = [0u8; 32];
            getrandom::fill(&mut secret).map_err(|e| LogError::Io(io::Error::other(e)))?;
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)?;
            file.write_all(&secret)?;
            file.sync_all()?;
            Ok(SigningKey::from_bytes(&secret))
        }
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

/// Every op in `text`, verified against `key` and its chain. A last line
/// with no newline is a write that never finished (its caller was never told
/// it succeeded), so it's left out; `torn` says where it starts.
fn verify(text: &str, key: &VerifyingKey) -> Result<(Vec<Op>, Option<usize>), LogError> {
    let (whole, torn) = match text.rfind('\n') {
        Some(end) if end + 1 == text.len() => (text, None),
        Some(end) => (&text[..=end], Some(end + 1)),
        None if text.is_empty() => (text, None),
        None => ("", Some(0)),
    };
    let mut ops: Vec<Op> = Vec::new();
    for (i, raw) in whole.lines().enumerate() {
        let corrupt = |reason: &str| LogError::Corrupt {
            line: i + 1,
            reason: reason.to_owned(),
        };
        let line: Line = serde_json::from_str(raw).map_err(|e| corrupt(&e.to_string()))?;
        if OpId::of(line.op.get().as_bytes()) != line.id {
            return Err(corrupt("id doesn't match the op"));
        }
        let sig = parse_sig(line.sig).ok_or_else(|| corrupt("signature isn't 64 hex bytes"))?;
        key.verify_strict(line.id.as_bytes(), &sig)
            .map_err(|_| corrupt("signature doesn't verify against the ledger's key"))?;
        let op: Unsigned =
            serde_json::from_str(line.op.get()).map_err(|e| corrupt(&e.to_string()))?;
        if op.parent != ops.last().map(|o| o.id) {
            return Err(corrupt("parent isn't the op before it"));
        }
        ops.push(Op { id: line.id, op });
    }
    Ok((ops, torn))
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
    match fs::read_to_string(dir.join(LOG_FILE)) {
        Ok(text) => Ok(verify(&text, key)?.0),
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
}

impl OpLog {
    /// Opens (or starts) the log at `dir`, verifies every op in it against
    /// `key`, and returns them with the log ready to append after them.
    pub fn open(dir: &Path, key: SigningKey) -> Result<(Self, Vec<Op>), LogError> {
        fs::create_dir_all(dir)?;
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
        let mut text = String::new();
        file.read_to_string(&mut text)?;
        let (ops, torn) = verify(&text, &key.verifying_key())?;
        if let Some(at) = torn {
            // Never a recorded op: its append didn't finish, so nobody was
            // told it happened. Cutting it keeps the next line whole.
            file.set_len(at as u64)?;
            file.sync_all()?;
        }
        file.seek(SeekFrom::End(0))?;
        let head = ops.last().map(|o| o.id);
        Ok((
            Self {
                file,
                _lock: lock,
                key,
                head,
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
        self.file.write_all(line.as_bytes())?;
        self.file.sync_data()?;
        self.head = Some(id);
        Ok(Op { id, op })
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
