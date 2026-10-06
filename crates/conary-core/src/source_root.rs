// crates/conary-core/src/source_root.rs

//! Source roots: named, Conary-managed runtime roots, one per source profile.
//!
//! A source root lives at `<base>/<name>/` (the host base is
//! `/var/lib/conary/roots`). It is an ordinary [`ConaryRuntimeRoot`] whose root
//! is that directory and whose database is `<base>/<name>/conary.db`, so its
//! database, CAS (`objects/`), generations, `etc-state/`, GC roots, and keyring
//! (`keys/`) are disjoint from the host's and from every other root's.
//!
//! The root's database pins its source identity in the transaction that
//! creates the schema (`source_root_identity`, schema revision 58). The pin is
//! immutable. The registry directory is self-describing: a root is an
//! authority only when its directory is a real `0700` directory owned by the
//! registry owner, its name satisfies [`SourceRootName`], its database has the
//! current schema, and its pin equals its name. Anything else is listed as a
//! typed [`SourceRootNonAuthority`] and is never opened for package work or
//! reinterpreted for compatibility.
//!
//! This module only addresses roots. The CLI does not use it yet; install,
//! rollback, and launch through source roots belong to later slices.
//!
//! [`ConaryRuntimeRoot`]: crate::runtime_root::ConaryRuntimeRoot

mod name;
mod pin;
mod registry;

pub use name::{SOURCE_ROOT_NAME_MAX_LEN, SourceRootName, SourceRootNameError};
pub use registry::{
    DEFAULT_SOURCE_ROOTS_BASE, RegistryBaseDefect, SOURCE_ROOT_DB_FILE, SOURCE_ROOT_DIR_MODE,
    SOURCE_ROOTS_BASE_MODE, SourceRoot, SourceRootEntry, SourceRootError, SourceRootNonAuthority,
    SourceRootRegistry,
};

#[cfg(test)]
mod tests;
