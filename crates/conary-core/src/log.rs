//! The op log (`docs/DESIGN.md` sections 4.6 and 8): one append-only file,
//! one operation per line, each hash-chained to the one before it and signed
//! by the ledger's key. It's the ledger's source of truth; everything else is
//! rebuilt by replaying it.
//!
//! A line is `{"id":…,"sig":…,"op":{…}}`. The id is the hash of the `op`
//! bytes exactly as written, so verifying a line never depends on how a
//! later build would serialize it. A signature is checked once for the
//! same bytes: `verified` says how a later read knows.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::Deserialize;
use serde_json::value::RawValue;

use crate::id::OpId;
use crate::op::{Body, Op, Unsigned};

mod verified;
use verified::Mark;

const LOG_FILE: &str = "ops.jsonl";
const LOCK_FILE: &str = "lock";
const ANCHOR_FILE: &str = "anchor";
const KEY_FILE: &str = "key";

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
    /// writer thinks it does. Taking the lock again reconciles it.
    Poisoned,
    /// No anchor beside the log says which of its ops were written: the
    /// ledger was never set up, or the anchor file was removed or isn't one.
    NoAnchor(PathBuf),
    /// No signing key: a writer only ever uses the ledger's own, which
    /// `cn init` makes once.
    NoKey(PathBuf),
}

impl std::fmt::Display for LogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "op log: {e}"),
            Self::Corrupt { line, reason } => write!(f, "op log line {line}: {reason}"),
            Self::Key(path) => write!(f, "{}: not an ed25519 key", path.display()),
            Self::Unanchored(a) => write!(f, "op log: op {} isn't op {} of the log", a.op, a.seq),
            Self::Poisoned => f.write_str("op log: an earlier append failed; take the lock again"),
            Self::NoAnchor(path) => write!(
                f,
                "{}: no anchor for the op log; `cn init` sets a ledger up, and `cn verify` checks one against GitHub",
                path.display()
            ),
            Self::NoKey(path) => write!(
                f,
                "{}: no signing key; `cn init` makes a new ledger's, and a ledger that has one needs it back",
                path.display()
            ),
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

/// The ledger's signing key, from `path`, with its public half put back
/// beside it if that's gone or wrong. It's never made here: a key made in
/// place of a lost one would start another ledger.
pub fn load_signing_key(path: &Path) -> Result<SigningKey, LogError> {
    let key = match fs::read(path) {
        Ok(bytes) => load_key(path, bytes)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(LogError::NoKey(path.to_owned()));
        }
        Err(e) => return Err(e.into()),
    };
    write_public_key(path, &key)?;
    Ok(key)
}

/// The ledger's signing key: loaded from `path`, or made there (readable by
/// its owner only) the first time. Either way it's on disk, and named there,
/// before it's returned. Only setting a ledger up makes one.
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
    replace(&public, &text)
}

/// Puts `text` at `path` whole: written under a name of its own, synced and
/// renamed into place, so a reader never sees part of it.
fn replace(path: &Path, text: &str) -> Result<(), LogError> {
    let mut nonce = [0u8; 8];
    getrandom::fill(&mut nonce).map_err(|e| LogError::Io(io::Error::other(e)))?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}.new", hex(&nonce)));
    fs::write(&tmp, text)?;
    File::open(&tmp)?.sync_all()?;
    fs::rename(&tmp, path)?;
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

impl std::fmt::Display for Anchor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.seq, self.op)
    }
}

impl std::str::FromStr for Anchor {
    type Err = ();

    /// `SEQ OP`, as [`Anchor`] prints.
    fn from_str(s: &str) -> Result<Self, ()> {
        let (seq, op) = s.trim().split_once(' ').ok_or(())?;
        Ok(Self {
            seq: seq.parse().map_err(|_| ())?,
            op: op.parse().map_err(|_| ())?,
        })
    }
}

impl Anchor {
    /// Whether `ops`, a whole log, holds this anchor.
    pub fn check(self, ops: &[Op]) -> Result<(), LogError> {
        self.found(
            self.seq
                .checked_sub(1)
                .and_then(|i| ops.get(i))
                .map(|o| o.id),
        )
    }

    /// Whether `at`, the log's `seq`th op if it has one, is this anchor's.
    fn found(self, at: Option<OpId>) -> Result<(), LogError> {
        match at {
            Some(id) if id == self.op => Ok(()),
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

/// Verifies one line, the `n`th, as the op after `parent`: all of it, but
/// for the signature of one that's `marked` ([`Mark`]).
fn verify_line(
    n: usize,
    raw: &[u8],
    parent: Option<OpId>,
    key: &VerifyingKey,
    marked: bool,
) -> Result<Op, LogError> {
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
    if !marked {
        #[cfg(test)]
        tests::marks::checked();
        key.verify_strict(line.id.as_bytes(), &sig)
            .map_err(|_| corrupt("signature doesn't verify against the ledger's key"))?;
    }
    let op: Unsigned = serde_json::from_str(line.op.get()).map_err(|e| corrupt(&e.to_string()))?;
    if op.parent != parent {
        return Err(corrupt("parent isn't the op before it"));
    }
    Ok(Op { id: line.id, op })
}

/// Every op in the log's bytes, verified against `key` and its chain: each
/// signature, but for those of the bytes `mark` vouches for. The bytes may
/// be the log's from some line on: `before` ops come before them, the last
/// of them `parent`. A mark vouches only from the log's start, so such
/// bytes are verified with none.
fn verify(
    bytes: &[u8],
    key: &VerifyingKey,
    before: usize,
    parent: Option<OpId>,
    mark: Option<&Mark>,
) -> Result<(Vec<Op>, Tail), LogError> {
    let marked = mark.map_or(0, |m| m.covers(bytes, key));
    let end = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    let (whole, rest) = bytes.split_at(end);
    let mut ops: Vec<Op> = Vec::new();
    let lines = whole.strip_suffix(b"\n").map(|w| w.split(|&b| b == b'\n'));
    let mut line_end = 0;
    for (i, raw) in lines.into_iter().flatten().enumerate() {
        line_end += raw.len() + 1;
        let after = ops.last().map(|o| o.id).or(parent);
        let op = verify_line(before + i + 1, raw, after, key, line_end <= marked)?;
        ops.push(op);
    }
    if rest.is_empty() {
        return Ok((ops, Tail::Whole));
    }
    if is_cut_short(rest) {
        return Ok((ops, Tail::Torn { at: end }));
    }
    // Complete, so it verifies like any other line or the log is refused.
    let after = ops.last().map(|o| o.id).or(parent);
    let op = verify_line(before + ops.len() + 1, rest, after, key, false)?;
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

/// Every op in the log at `dir`, each signature verified against the
/// ledger's public key, and the log against the newest anchor its caller
/// holds, when there is one. It trusts nothing beside the log: it's the read
/// for setting a ledger up and for auditing one. A reader takes no lock: a
/// write still in progress is only a torn last line, which it leaves out.
pub fn read(dir: &Path, key: &VerifyingKey, anchor: Option<Anchor>) -> Result<Vec<Op>, LogError> {
    read_past(dir, key, anchor, None)
}

/// The same ops, for everyday use: a signature a writer already verified,
/// in bytes unchanged since, isn't verified again.
pub fn read_marked(
    dir: &Path,
    key: &VerifyingKey,
    anchor: Option<Anchor>,
) -> Result<Vec<Op>, LogError> {
    let mark = Mark::load(dir);
    read_past(dir, key, anchor, mark.as_ref())
}

fn read_past(
    dir: &Path,
    key: &VerifyingKey,
    anchor: Option<Anchor>,
    mark: Option<&Mark>,
) -> Result<Vec<Op>, LogError> {
    let ops = match fs::read(dir.join(LOG_FILE)) {
        Ok(bytes) => verify(&bytes, key, 0, None, mark)?.0,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.into()),
    };
    anchor.map_or(Ok(()), |a| a.check(&ops))?;
    Ok(ops)
}

const NO_OPS: &str = "none";

/// The copy of the anchor kept beside the log: the newest anchor its writer
/// checked the log against or published (`None` for a ledger set up with no
/// ops yet), and where the anchor kept off the box was when it did, in that
/// store's own terms. It's only as good as that writer's check: whoever can
/// cut the log can rewrite it too, so it spares readers a round trip to the
/// anchor kept off the box, and stands in for it only there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    pub anchor: Option<Anchor>,
    /// Missing in a copy kept before it was recorded.
    pub at: Option<String>,
}

/// Keeps the copy beside the log. Only a writer holding the [`Lock`] keeps
/// one, so it never goes back. `at` is one word.
pub fn save_anchor(lock: &Lock, anchor: Option<Anchor>, at: &str) -> Result<(), LogError> {
    if at.is_empty() || at.contains(char::is_whitespace) {
        return Err(LogError::Io(io::Error::other(format!(
            "{at:?} isn't where an anchor is"
        ))));
    }
    let text = anchor.map_or_else(|| NO_OPS.to_owned(), |a| a.to_string());
    replace(&lock.dir.join(ANCHOR_FILE), &format!("{text} {at}\n"))
}

/// The copy [`save_anchor`] kept. None is kept for a ledger never set up, or
/// one whose copy was removed, so that's refused.
pub fn load_checked(dir: &Path) -> Result<Checked, LogError> {
    let path = dir.join(ANCHOR_FILE);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(LogError::NoAnchor(path)),
        Err(e) => return Err(e.into()),
    };
    let bad = || LogError::NoAnchor(path.clone());
    let words: Vec<&str> = text.split_whitespace().collect();
    let (anchor, rest) = match words.as_slice() {
        [NO_OPS, rest @ ..] => (None, rest),
        [seq, op, rest @ ..] => (
            Some(format!("{seq} {op}").parse().map_err(|()| bad())?),
            rest,
        ),
        _ => return Err(bad()),
    };
    match rest {
        [] => Ok(Checked { anchor, at: None }),
        [at] => Ok(Checked {
            anchor,
            at: Some((*at).to_owned()),
        }),
        _ => Err(bad()),
    }
}

/// The anchor in the copy [`save_anchor`] kept.
pub fn load_anchor(dir: &Path) -> Result<Option<Anchor>, LogError> {
    load_checked(dir).map(|c| c.anchor)
}

/// Where the ledger is when nothing says otherwise: `conary` in the XDG
/// state directory.
pub fn default_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("conary")
}

/// The signing key's file in the ledger at `dir`. Its public key is beside
/// it, at [`public_path`].
pub fn key_path(dir: &Path) -> PathBuf {
    dir.join(KEY_FILE)
}

/// The log's file in the ledger at `dir`.
pub fn log_path(dir: &Path) -> PathBuf {
    dir.join(LOG_FILE)
}

/// The anchor copy's file in the ledger at `dir`.
pub fn anchor_path(dir: &Path) -> PathBuf {
    dir.join(ANCHOR_FILE)
}

/// Whether there's an anchor file beside the log at `dir`, readable or not.
pub fn has_anchor(dir: &Path) -> bool {
    dir.join(ANCHOR_FILE).exists()
}

/// Every op in the log at `dir`, checked against the anchor kept beside it.
pub fn read_anchored(dir: &Path, key: &VerifyingKey) -> Result<Vec<Op>, LogError> {
    let anchor = load_anchor(dir)?;
    read_marked(dir, key, anchor)
}

/// The ledger's writer lock, on the lock file in its directory: one writer
/// at a time, until it's dropped. A writer takes it before it reads what it
/// checks the log against, so neither can change under it.
pub struct Lock {
    dir: PathBuf,
    _file: File,
}

impl Lock {
    /// Makes `dir` if it isn't there yet, and waits for any other writer.
    pub fn take(dir: &Path) -> Result<Self, LogError> {
        publish_dir(dir)?;
        Self::again(dir)
    }

    /// Waits for any other writer, in a directory the lock was taken in
    /// before: each [`OpLog::writer`] takes it this way, without syncing the
    /// directories on the way down again.
    fn again(dir: &Path) -> Result<Self, LogError> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(LOCK_FILE))?;
        file.lock()?;
        Ok(Self {
            dir: dir.to_owned(),
            _file: file,
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// The log, open for appending. It holds its directory's [`Lock`] only
/// while it writes ([`OpLog::writer`]), so other writers append between its
/// writes, and each time it takes the lock back it reads what they appended.
pub struct OpLog {
    file: File,
    dir: PathBuf,
    key: SigningKey,
    /// Every op's id, in order: what an anchor is checked against without
    /// reading the log again.
    ids: Vec<OpId>,
    /// The bytes of the file read and verified: where another writer's ops
    /// start.
    len: u64,
    /// The lock, while its caller holds it across writes ([`OpLog::hold`]).
    held: Option<Lock>,
    poisoned: bool,
    #[cfg(test)]
    fail_next_write: bool,
}

/// Makes the file end on a whole line after verifying bytes from `from` on,
/// and returns where it ends.
fn mend(file: &mut File, from: u64, bytes: &[u8], tail: Tail) -> io::Result<u64> {
    let end = from + bytes.len() as u64;
    match tail {
        Tail::Whole => Ok(end),
        Tail::Unterminated => {
            file.write_all(b"\n")?;
            file.sync_data()?;
            Ok(end + 1)
        }
        Tail::Torn { at } => {
            // Never a recorded op: its append didn't finish, so nobody
            // was told it happened. Cutting it keeps the next line whole.
            let at = from + at as u64;
            file.set_len(at)?;
            file.sync_all()?;
            Ok(at)
        }
    }
}

impl OpLog {
    /// Opens (or starts) the log in `lock`'s directory, verifies every op in
    /// it against `key` and, when there is one, the newest anchor its caller
    /// holds, and returns them with the log ready to append after them.
    /// Nothing in the file changes until it has verified. What verified is
    /// marked, so the next open checks only the signatures after it. The lock
    /// is let go once it's open: each write takes it again.
    pub fn open(
        lock: Lock,
        key: SigningKey,
        anchor: Option<Anchor>,
    ) -> Result<(Self, Vec<Op>), LogError> {
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
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let mark = Mark::load(&dir);
        let (ops, tail) = verify(&bytes, &key.verifying_key(), 0, None, mark.as_ref())?;
        anchor.map_or(Ok(()), |a| a.check(&ops))?;
        verified::keep(&dir, &bytes, &key, mark)?;
        let len = mend(&mut file, 0, &bytes, tail)?;
        Ok((
            Self {
                file,
                dir,
                key,
                ids: ops.iter().map(|o| o.id).collect(),
                len,
                held: None,
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

    /// The ledger's directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The last op written, to anchor the log at.
    pub fn head(&self) -> Option<Anchor> {
        self.ids.last().map(|&op| Anchor {
            seq: self.ids.len(),
            op,
        })
    }

    /// Takes the lock again, waiting for any other writer, and returns the
    /// ops other writers appended since this log last held it, verified as
    /// the ones after its head: only the bytes past what it read before are
    /// read, so a write never costs a read of the whole log. A log cut
    /// shorter than that, or whose next line isn't the op after the head, is
    /// refused. An append of this log's own that failed is reconciled here,
    /// the way opening the log reconciles one.
    pub fn writer(&mut self) -> Result<(Writer<'_>, Vec<Op>), LogError> {
        let lock = match self.held {
            Some(_) => None,
            None => Some(Lock::again(&self.dir)?),
        };
        let ops = self.catch_up()?;
        Ok((Writer { log: self, lock }, ops))
    }

    /// Takes the lock as [`OpLog::writer`] does and keeps it, every write
    /// made under it, until [`OpLog::release`] or the log is dropped. Held
    /// already, it reads nothing new. When what was appended can't be read,
    /// a lock this call took is let go.
    pub fn hold(&mut self) -> Result<Vec<Op>, LogError> {
        let took = self.held.is_none();
        if took {
            self.held = Some(Lock::again(&self.dir)?);
        }
        let ops = self.catch_up();
        if ops.is_err() && took {
            self.held = None;
        }
        ops
    }

    /// Lets the lock [`OpLog::hold`] took go.
    pub fn release(&mut self) {
        self.held = None;
    }

    /// Whether it holds the lock ([`OpLog::hold`]).
    pub fn holds(&self) -> bool {
        self.held.is_some()
    }

    fn catch_up(&mut self) -> Result<Vec<Op>, LogError> {
        let size = self.file.metadata()?.len();
        if size < self.len
            && let Some(head) = self.head()
        {
            return Err(LogError::Unanchored(head));
        }
        if size == self.len {
            self.poisoned = false;
            return Ok(Vec::new());
        }
        // Another writer's ops, before any is acknowledged: it may have
        // failed to sync them.
        self.file.sync_all()?;
        self.file.seek(SeekFrom::Start(self.len))?;
        let mut bytes = Vec::new();
        (&self.file).read_to_end(&mut bytes)?;
        let head = self.head();
        let (ops, tail) = verify(
            &bytes,
            &self.key.verifying_key(),
            self.ids.len(),
            head.map(|h| h.op),
            None,
        )?;
        self.len = mend(&mut self.file, self.len, &bytes, tail)?;
        self.ids.extend(ops.iter().map(|o| o.id));
        self.poisoned = false;
        Ok(ops)
    }
}

/// The log under its lock: what [`OpLog::writer`] returns. The lock goes
/// when it's dropped, unless the log holds it ([`OpLog::hold`]).
pub struct Writer<'a> {
    log: &'a mut OpLog,
    /// The lock this writer took; none while the log holds its own.
    lock: Option<Lock>,
}

impl Writer<'_> {
    pub fn lock(&self) -> &Lock {
        self.lock
            .as_ref()
            .or(self.log.held.as_ref())
            .expect("a writer has the lock or its log holds it")
    }

    /// The last op written, to anchor the log at.
    pub fn head(&self) -> Option<Anchor> {
        self.log.head()
    }

    /// Whether its log holds the lock across writes ([`OpLog::hold`]),
    /// rather than this writer for its own.
    pub fn held(&self) -> bool {
        self.lock.is_none()
    }

    /// Whether the log holds `anchor`, as it stands under this lock.
    pub fn check(&self, anchor: Anchor) -> Result<(), LogError> {
        anchor.found(
            anchor
                .seq
                .checked_sub(1)
                .and_then(|i| self.log.ids.get(i))
                .copied(),
        )
    }

    /// Appends one op after the last, signed, and returns it once it's on
    /// disk.
    pub fn append(&mut self, body: Body, at: i64) -> Result<Op, LogError> {
        let log = &mut *self.log;
        if log.poisoned {
            return Err(LogError::Poisoned);
        }
        let op = Unsigned {
            parent: log.ids.last().copied(),
            at,
            body,
        };
        let json = serde_json::to_string(&op).map_err(|e| LogError::Io(io::Error::other(e)))?;
        let id = OpId::of(json.as_bytes());
        let sig = log.key.sign(id.as_bytes());
        let line = format!(
            "{{\"id\":\"{id}\",\"sig\":\"{}\",\"op\":{json}}}\n",
            hex(&sig.to_bytes())
        );
        if let Err(e) = log.write_line(line.as_bytes()) {
            // The line may be on disk whole, in part or not at all, and only
            // reading it again, under the lock, can tell which.
            log.poisoned = true;
            return Err(e.into());
        }
        log.ids.push(id);
        log.len += line.len() as u64;
        Ok(Op { id, op })
    }
}

impl OpLog {
    fn write_line(&mut self, line: &[u8]) -> io::Result<()> {
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
