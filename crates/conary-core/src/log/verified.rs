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

fn message(prefix: &[u8]) -> String {
    let hash = blake3::hash(prefix).to_hex();
    format!("{HEADER}\n{} {hash}\n", prefix.len())
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

    /// How many bytes from the start of `log` this mark vouches for under
    /// `key`: all it covers, or none.
    pub(super) fn covers(&self, log: &[u8], key: &VerifyingKey) -> usize {
        log.get(..self.len)
            .filter(|prefix| {
                key.verify_strict(message(prefix).as_bytes(), &self.sig)
                    .is_ok()
            })
            .map_or(0, <[u8]>::len)
    }
}

/// Marks the whole lines of `log`, which its caller holds the lock on and
/// has just verified under `key`, in place of `was`.
pub(super) fn keep(
    dir: &Path,
    log: &[u8],
    key: &SigningKey,
    was: Option<Mark>,
) -> Result<(), LogError> {
    let Some(last) = log.iter().rposition(|&b| b == b'\n') else {
        return Ok(()); // No whole line to mark.
    };
    let prefix = &log[..=last];
    let mark = Mark {
        len: prefix.len(),
        sig: key.sign(message(prefix).as_bytes()),
    };
    if was.as_ref() == Some(&mark) {
        return Ok(());
    }
    let text = format!("{} {}\n", mark.len, hex(&mark.sig.to_bytes()));
    replace(&dir.join(MARK_FILE), &text)
}

#[cfg(test)]
pub(super) mod tests;
