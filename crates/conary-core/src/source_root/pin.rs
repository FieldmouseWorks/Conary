// crates/conary-core/src/source_root/pin.rs

//! The immutable source-identity pin stored in a source root's database.
//!
//! The only writer is [`initialize_pinned_database`], which records the pin in
//! the transaction that creates the schema. There is no update or delete path
//! in code, and the `source_root_identity` triggers refuse both in SQL.

use super::name::{SourceRootName, SourceRootNameError};
use crate::error::Result;
use rusqlite::Connection;
use std::path::Path;

/// The pin as stored, before it is validated against a directory name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum StoredPin {
    /// The database has no pin row (for example, the host database).
    Missing,
    /// The stored identity does not satisfy the name grammar.
    Malformed {
        identity: String,
        error: SourceRootNameError,
    },
    Present {
        identity: SourceRootName,
        created_at: String,
    },
}

/// Create the current schema in a fresh database and record `identity` as its
/// pin in the same transaction.
pub(super) fn initialize_pinned_database(db_path: &Path, identity: &SourceRootName) -> Result<()> {
    crate::db::init_fresh_with(db_path, |tx| {
        tx.execute(
            "INSERT INTO source_root_identity (singleton, identity) VALUES (1, ?1)",
            [identity.as_str()],
        )?;
        Ok(())
    })
}

/// Read the pin from a current-schema database.
///
/// The caller must already have proven the schema current; a malformed stored
/// identity is reported, never coerced.
pub(super) fn read_pin(conn: &Connection) -> Result<StoredPin> {
    let mut statement =
        conn.prepare("SELECT identity, created_at FROM source_root_identity ORDER BY singleton")?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // The singleton primary key admits at most one row.
    let Some((identity, created_at)) = rows.into_iter().next() else {
        return Ok(StoredPin::Missing);
    };
    Ok(match SourceRootName::parse(&identity) {
        Ok(name) => StoredPin::Present {
            identity: name,
            created_at,
        },
        Err(error) => StoredPin::Malformed { identity, error },
    })
}
