//! The mark beside the log: how many of its bytes a writer verified, and
//! the ledger key's signature over that length and those bytes' hash.
//! Checking a signature is most of what reading an op costs, and these
//! exact bytes under this key can only check the same way again, so a read
//! that trusts the mark checks the signatures of the lines after it alone.
//!
//! The mark lets nothing through that a full read refuses. It counts only
//! for a prefix that hashes as signed: change, cut or re-sign any byte of it
//! and every signature is checked. Making one takes the signing key, which
//! could as well sign each op. Each line's id, parent and schema are checked
//! on every read, marked or not, and so is the anchor. [`super::read`],
//! which `cn init` and `cn verify` use, never takes one.

use std::fs;
use std::path::Path;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

use super::{LogError, hex, parse_sig, replace};

const MARK_FILE: &str = "verified";
/// Opens every mark's signed message, which so is never an op id's 32 bytes.
const HEADER: &str = "conary op log verified prefix v1";

fn message_for(len: usize, hash: &blake3::Hash) -> String {
    format!("{HEADER}\n{len} {}\n", hash.to_hex())
}

#[derive(PartialEq, Eq)]
pub(super) struct Mark {
    len: usize,
    sig: Signature,
}

impl Mark {
    /// The mark in `dir`, or none to go by. A reader loads it before it
    /// reads the log: one kept after could cover more than the reader read.
    pub(super) fn load(dir: &Path) -> Option<Self> {
        let text = fs::read_to_string(dir.join(MARK_FILE)).ok()?;
        let (len, sig) = text.trim_end().split_once(' ')?;
        Some(Self {
            len: len.parse().ok()?,
            sig: parse_sig(sig)?,
        })
    }

    pub(super) fn len(&self) -> usize {
        self.len
    }

    /// Whether this mark vouches for the `self.len` bytes whose BLAKE3 digest
    /// is `hash`, under `key`.
    pub(super) fn verify_hash(&self, hash: &blake3::Hash, key: &VerifyingKey) -> bool {
        key.verify_strict(message_for(self.len, hash).as_bytes(), &self.sig)
            .is_ok()
    }

    /// How many bytes from the start of `log` this mark vouches for under
    /// `key`: all it covers, or none.
    pub(super) fn covers(&self, log: &[u8], key: &VerifyingKey) -> usize {
        log.get(..self.len)
            .filter(|prefix| self.verify_hash(&blake3::hash(prefix), key))
            .map_or(0, <[u8]>::len)
    }
}

/// Marks the first `len` bytes of the log (whole lines ending on a newline),
/// whose BLAKE3 digest is `hash` and which its caller holds the lock on and
/// has verified under `key`, in place of `was`.
pub(super) fn keep_hash(
    dir: &Path,
    len: usize,
    hash: &blake3::Hash,
    key: &SigningKey,
    was: Option<Mark>,
) -> Result<(), LogError> {
    if len == 0 {
        return Ok(()); // No whole line to mark.
    }
    let mark = Mark {
        len,
        sig: key.sign(message_for(len, hash).as_bytes()),
    };
    if was.as_ref() == Some(&mark) {
        return Ok(());
    }
    let text = format!("{} {}\n", mark.len, hex(&mark.sig.to_bytes()));
    replace(&dir.join(MARK_FILE), &text)
}
