// crates/conary-core/src/source_root/name.rs

//! The typed name of a source root.

use std::fmt;

/// Longest accepted source-root name, in bytes.
pub const SOURCE_ROOT_NAME_MAX_LEN: usize = 64;

/// A validated source-root name.
///
/// The name is both the source identity pinned in the root's database and the
/// root's directory name under the registry base, so it must be a valid source
/// identity (see
/// [`validate_source_identity`](crate::repository::resolution_policy::validate_source_identity))
/// and a safe single path component. The grammar is:
///
/// ```text
/// name  = first rest{0,63}
/// first = [a-z0-9]
/// rest  = [a-z0-9] | "." | "_" | "-"
/// ```
///
/// Requiring an alphanumeric first byte excludes `.`, `..`, hidden or staging
/// directories, and option-like names; excluding `/` and NUL keeps the name a
/// single component. Upper case is excluded so names never differ only by
/// case. Every catalogued profile (`fedora-44`, `ubuntu-26.04`, `arch`) fits.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceRootName(String);

/// Why a string is not a [`SourceRootName`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SourceRootNameError {
    #[error("source root name is empty")]
    Empty,
    #[error("source root name is {len} bytes; at most {SOURCE_ROOT_NAME_MAX_LEN} are allowed")]
    TooLong { len: usize },
    #[error("source root name must start with [a-z0-9], found {found:?}")]
    InvalidFirstByte { found: char },
    #[error("source root name contains {found:?} at byte {index}; only [a-z0-9._-] is allowed")]
    InvalidByte { index: usize, found: char },
    #[error("source root directory name is not valid UTF-8")]
    NotUtf8,
    #[error("source root name is not a valid source identity: {0}")]
    InvalidSourceIdentity(String),
}

impl SourceRootName {
    /// Parse a source-root name, rejecting anything outside the grammar.
    pub fn parse(value: &str) -> Result<Self, SourceRootNameError> {
        let bytes = value.as_bytes();
        let Some(&first) = bytes.first() else {
            return Err(SourceRootNameError::Empty);
        };
        if bytes.len() > SOURCE_ROOT_NAME_MAX_LEN {
            return Err(SourceRootNameError::TooLong { len: bytes.len() });
        }
        if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
            return Err(SourceRootNameError::InvalidFirstByte {
                found: first_char(value),
            });
        }
        for (index, ch) in value.char_indices() {
            let allowed =
                ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '.' | '_' | '-');
            if !allowed {
                return Err(SourceRootNameError::InvalidByte { index, found: ch });
            }
        }
        crate::repository::resolution_policy::validate_source_identity(value, "source root name")
            .map_err(SourceRootNameError::InvalidSourceIdentity)?;
        Ok(Self(value.to_string()))
    }

    /// Parse a directory entry name from the registry.
    pub fn from_os_str(value: &std::ffi::OsStr) -> Result<Self, SourceRootNameError> {
        Self::parse(value.to_str().ok_or(SourceRootNameError::NotUtf8)?)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn first_char(value: &str) -> char {
    value.chars().next().unwrap_or('\0')
}

impl fmt::Display for SourceRootName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for SourceRootName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}
