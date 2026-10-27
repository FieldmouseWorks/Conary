//! The ids that name things in the ledger (`docs/DESIGN.md` sections 4.1,
//! 4.2 and 4.6). A change id is random and assigned by Conary; revision and op
//! ids are hashes of what they name, so they can't drift from it.

use std::fmt;
use std::str::FromStr;

/// Why a string isn't a valid id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseIdError {
    /// Wrong number of characters for this kind of id.
    Length { expected: &'static str, got: usize },
    /// A character outside the id's alphabet.
    Character(char),
    /// More bits than a change id holds.
    Overflow,
}

impl fmt::Display for ParseIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Length { expected, got } => {
                write!(f, "expected {expected} characters, got {got}")
            }
            Self::Character(c) => write!(f, "unexpected character {c:?}"),
            Self::Overflow => f.write_str("too large for a change id"),
        }
    }
}

impl std::error::Error for ParseIdError {}

/// Lowercase Crockford base32: no `i`, `l`, `o` or `u`, so an id read aloud
/// or typed by hand has fewer ways to go wrong.
const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";
/// 26 characters of 5 bits carry 130 bits, enough for 128.
const CHANGE_ID_LEN: usize = 26;

/// A change's id: 128 random bits, assigned by Conary when it first sees the
/// change and never changed after. Only [`ChangeId::new`] makes a fresh one,
/// so no author can choose theirs; parsing reads back one Conary already
/// assigned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChangeId(u128);

impl ChangeId {
    /// A fresh id from the operating system's random source.
    pub fn new() -> Result<Self, getrandom::Error> {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes)?;
        Ok(Self(u128::from_be_bytes(bytes)))
    }
}

impl fmt::Display for ChangeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = [0u8; CHANGE_ID_LEN];
        for (i, c) in out.iter_mut().enumerate() {
            let shift = 5 * (CHANGE_ID_LEN - 1 - i);
            *c = ALPHABET[((self.0 >> shift) & 0x1f) as usize];
        }
        // Every byte comes from ALPHABET, which is ASCII.
        f.write_str(std::str::from_utf8(&out).map_err(|_| fmt::Error)?)
    }
}

impl FromStr for ChangeId {
    type Err = ParseIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != CHANGE_ID_LEN {
            return Err(ParseIdError::Length {
                expected: "26",
                got: s.len(),
            });
        }
        let mut value: u128 = 0;
        for c in s.chars() {
            let digit = ALPHABET
                .iter()
                .position(|&a| char::from(a) == c)
                .ok_or(ParseIdError::Character(c))?;
            if value >> 123 != 0 {
                return Err(ParseIdError::Overflow);
            }
            value = (value << 5) | digit as u128;
        }
        Ok(Self(value))
    }
}

/// A git commit id: lowercase hex, 40 characters for SHA-1 repositories or 64
/// for SHA-256 ones.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CommitId(String);

impl CommitId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CommitId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for CommitId {
    type Err = ParseIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != 40 && s.len() != 64 {
            return Err(ParseIdError::Length {
                expected: "40 or 64",
                got: s.len(),
            });
        }
        if let Some(c) = s.chars().find(|c| !matches!(c, '0'..='9' | 'a'..='f')) {
            return Err(ParseIdError::Character(c));
        }
        Ok(Self(s.to_owned()))
    }
}

/// The context string for revision ids. Changing it changes every revision
/// id there is, so it's frozen; a new id format gets a new context.
const REVISION_CONTEXT: &str = "conary 2026-10-27 revision id v1";

/// A revision's id: the BLAKE3 hash of its base and head commit ids. A git
/// commit id already covers its tree, parents and message, so a reworded
/// commit, new ancestry, a rebase or a retarget all give a new revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RevisionId([u8; 32]);

impl RevisionId {
    pub fn new(base: &CommitId, head: &CommitId) -> Self {
        let mut hasher = blake3::Hasher::new_derive_key(REVISION_CONTEXT);
        // Each id carries its length, so no two (base, head) pairs share an
        // input, whichever hash algorithm each repository uses.
        for commit in [base, head] {
            let bytes = commit.as_str().as_bytes();
            hasher.update(&(bytes.len() as u64).to_le_bytes());
            hasher.update(bytes);
        }
        Self(*hasher.finalize().as_bytes())
    }
}

/// An operation's id: the BLAKE3 hash of the operation's bytes as the op log
/// holds them (`op.rs`), so an op that's been tampered with no longer matches
/// its id, and the next op's parent no longer matches either.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OpId([u8; 32]);

/// The context string for op ids, frozen like [`REVISION_CONTEXT`].
const OP_CONTEXT: &str = "conary 2026-10-27 op id v1";

impl OpId {
    pub fn of(bytes: &[u8]) -> Self {
        Self(blake3::derive_key(OP_CONTEXT, bytes))
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// The 32-byte ids print as 64 lowercase hex characters.
macro_rules! hex32 {
    ($($id:ident),*) => {$(
        impl fmt::Display for $id {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
            }
        }

        impl FromStr for $id {
            type Err = ParseIdError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                if s.len() != 64 {
                    return Err(ParseIdError::Length { expected: "64", got: s.len() });
                }
                let mut out = [0u8; 32];
                for (i, c) in s.chars().enumerate() {
                    let nibble = match c {
                        '0'..='9' => c as u8 - b'0',
                        'a'..='f' => c as u8 - b'a' + 10,
                        _ => return Err(ParseIdError::Character(c)),
                    };
                    out[i / 2] |= nibble << if i % 2 == 0 { 4 } else { 0 };
                }
                Ok(Self(out))
            }
        }
    )*};
}
hex32!(RevisionId, OpId);

/// Every id is stored as its text, so the op log reads the same in a terminal
/// as it does to Conary.
macro_rules! as_text {
    ($($id:ident),*) => {$(
        impl serde::Serialize for $id {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.collect_str(self)
            }
        }

        impl<'de> serde::Deserialize<'de> for $id {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let text = <std::borrow::Cow<'de, str>>::deserialize(d)?;
                text.parse().map_err(serde::de::Error::custom)
            }
        }
    )*};
}
as_text!(ChangeId, CommitId, RevisionId, OpId);

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(s: &str) -> CommitId {
        s.parse().expect("valid commit id")
    }

    const A: &str = "1111111111111111111111111111111111111111";
    const B: &str = "2222222222222222222222222222222222222222";

    #[test]
    fn change_ids_round_trip_through_their_text() {
        for id in [ChangeId(0), ChangeId(u128::MAX), ChangeId::new().unwrap()] {
            let text = id.to_string();
            assert_eq!(text.len(), CHANGE_ID_LEN);
            assert_eq!(text.parse::<ChangeId>(), Ok(id));
        }
    }

    #[test]
    fn fresh_change_ids_differ() {
        assert_ne!(ChangeId::new().unwrap(), ChangeId::new().unwrap());
    }

    #[test]
    fn change_id_text_outside_the_alphabet_or_range_is_refused() {
        assert_eq!(
            "0".repeat(25).parse::<ChangeId>(),
            Err(ParseIdError::Length {
                expected: "26",
                got: 25
            })
        );
        let with_i = format!("{}i", "0".repeat(25));
        assert_eq!(
            with_i.parse::<ChangeId>(),
            Err(ParseIdError::Character('i'))
        );
        // The first character carries 3 bits; 8 or more is past 128.
        let too_big = format!("8{}", "0".repeat(25));
        assert_eq!(too_big.parse::<ChangeId>(), Err(ParseIdError::Overflow));
    }

    #[test]
    fn commit_ids_must_be_lowercase_hex_of_a_git_length() {
        assert!(A.parse::<CommitId>().is_ok());
        assert!("a".repeat(64).parse::<CommitId>().is_ok());
        assert!("a".repeat(41).parse::<CommitId>().is_err());
        assert_eq!(
            A.to_uppercase().replace('1', "A").parse::<CommitId>(),
            Err(ParseIdError::Character('A'))
        );
    }

    #[test]
    fn a_revision_is_named_by_both_commits_in_order() {
        let ab = RevisionId::new(&commit(A), &commit(B));
        assert_eq!(ab, RevisionId::new(&commit(A), &commit(B)));
        assert_ne!(ab, RevisionId::new(&commit(B), &commit(A)));
        assert_ne!(ab, RevisionId::new(&commit(A), &commit(A)));
    }

    #[test]
    fn hex_ids_round_trip_and_refuse_bad_text() {
        let id = RevisionId::new(&commit(A), &commit(B));
        assert_eq!(id.to_string().parse::<RevisionId>(), Ok(id));
        assert_eq!(
            "A".repeat(64).parse::<OpId>(),
            Err(ParseIdError::Character('A'))
        );
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(serde_json::from_str::<RevisionId>(&json).unwrap(), id);
    }

    /// Ids in the ledger outlive any one build, so the format is frozen: if
    /// this fails, the change broke every revision id already recorded. The
    /// value was checked against `b3sum --derive-key` over the same input.
    #[test]
    fn revision_id_format_is_frozen() {
        assert_eq!(
            RevisionId::new(&commit(A), &commit(B)).to_string(),
            "926afea717635b8d3de886cb4c5accc6801c14c2f79bf32ed885a8cf947f3662"
        );
    }
}
