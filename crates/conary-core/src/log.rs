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
    /// The key file isn't a 32-byte ed25519 secret key, or the public key
    /// file isn't its 64 hex digits.
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
            Self::Key(path) => write!(f, "{}: not an ed25519 key", path.display()),
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
    write_public_key(path, &key)?;
    Ok(key)
}

/// Keeps the key's public half beside it, at [`public_path`], for readers
/// that only verify: renamed into place, so a reader never sees part of it.
fn write_public_key(path: &Path, key: &SigningKey) -> Result<(), LogError> {
    let public = public_path(path);
    let text = format!("{}\n", hex(key.verifying_key().as_bytes()));
    if fs::read_to_string(&public).ok().as_deref() == Some(text.as_str()) {
        return Ok(());
    }
    let mut nonce = [0u8; 8];
    getrandom::fill(&mut nonce).map_err(|e| LogError::Io(io::Error::other(e)))?;
    let mut tmp = public.clone().into_os_string();
    tmp.push(format!(".{}.new", hex(&nonce)));
    fs::write(&tmp, text)?;
    File::open(&tmp)?.sync_all()?;
    fs::rename(&tmp, &public)?;
    sync_dir(parent_of(path))?;
    Ok(())
}

/// Where the public half of the key at `path` is kept.
pub fn public_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".pub");
    PathBuf::from(name)
}

/// The ledger's public key, from the file [`load_or_create_key`] keeps.
pub fn load_public_key(path: &Path) -> Result<VerifyingKey, LogError> {
    let bad = || LogError::Key(path.to_owned());
    let text = fs::read_to_string(path)?;
    let text = text.trim();
    if text.len() != 64 {
        return Err(bad());
    }
    let mut bytes = [0u8; 32];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(2 * i..2 * i + 2).ok_or_else(bad)?, 16)
            .map_err(|_| bad())?;
    }
    VerifyingKey::from_bytes(&bytes).map_err(|_| bad())
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
mod tests;
