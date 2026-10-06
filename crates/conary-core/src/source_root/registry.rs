// crates/conary-core/src/source_root/registry.rs

//! The on-disk registry of source roots.

use super::name::{SourceRootName, SourceRootNameError};
use super::pin::{StoredPin, initialize_pinned_database, read_pin};
use crate::db::schema::{SchemaCompatibility, inspect};
use crate::runtime_root::ConaryRuntimeRoot;
use rusqlite::{Connection, OpenFlags};
use std::fs::{DirBuilder, Permissions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Registry base on a host: one directory per source root.
pub const DEFAULT_SOURCE_ROOTS_BASE: &str = "/var/lib/conary/roots";
/// Database file name inside each source root.
pub const SOURCE_ROOT_DB_FILE: &str = "conary.db";
/// Exact permission bits of every source-root directory.
pub const SOURCE_ROOT_DIR_MODE: u32 = 0o700;
/// Permission bits given to a registry base this code creates.
///
/// The base must stay traversable so the unprivileged launcher can later
/// reach each root's world-traversable launch trees; root names are public
/// profile identities, so listing them discloses nothing. Each root directory
/// itself is `0700`.
pub const SOURCE_ROOTS_BASE_MODE: u32 = 0o755;
/// Prefix of a root being created. It is outside the name grammar, so an
/// interrupted creation is listed as an invalid name, never as a root.
const STAGING_PREFIX: &str = ".creating-";

/// A validated source root: its name, pinned identity, and runtime root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRoot {
    name: SourceRootName,
    pinned_at: String,
    runtime_root: ConaryRuntimeRoot,
}

impl SourceRoot {
    pub fn name(&self) -> &SourceRootName {
        &self.name
    }

    /// The identity pinned in the root's database. It always equals the name;
    /// a root whose pin differs is never returned.
    pub fn pinned_identity(&self) -> &SourceRootName {
        &self.name
    }

    /// When the pin was recorded (SQLite `CURRENT_TIMESTAMP`, UTC).
    pub fn pinned_at(&self) -> &str {
        &self.pinned_at
    }

    pub fn runtime_root(&self) -> &ConaryRuntimeRoot {
        &self.runtime_root
    }

    pub fn root_dir(&self) -> &Path {
        self.runtime_root.root()
    }

    pub fn db_path(&self) -> &Path {
        self.runtime_root.db_path()
    }
}

/// Why a registry directory entry is not a source-root authority.
///
/// Every variant is fencing or rebuild state: the entry is reported, never
/// opened for package work, and never reinterpreted for compatibility.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SourceRootNonAuthority {
    #[error("directory name is not a source root name: {0}")]
    InvalidName(SourceRootNameError),
    #[error("entry is not a real directory")]
    NotADirectory,
    #[error("directory is owned by uid {uid}; expected uid {expected}")]
    WrongOwner { uid: u32, expected: u32 },
    #[error("directory mode is {mode:04o}; expected {SOURCE_ROOT_DIR_MODE:04o}")]
    UnsafePermissions { mode: u32 },
    #[error("root has no database")]
    MissingDatabase,
    #[error("root database is not a regular file")]
    DatabaseNotRegularFile,
    #[error("root database has no schema")]
    UninitializedDatabase,
    #[error("root database cannot be read: {error}")]
    UnreadableDatabase { error: String },
    #[error("root database requires a rebuild: {observed}")]
    SchemaRebuildRequired { observed: String },
    #[error("root database has no pinned source identity")]
    MissingPin,
    #[error("root database pins malformed identity {identity:?}: {error}")]
    MalformedPin {
        identity: String,
        error: SourceRootNameError,
    },
    #[error("root database pins identity {pinned}, which does not match its directory name")]
    PinMismatch { pinned: SourceRootName },
}

/// One entry of [`SourceRootRegistry::list`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceRootEntry {
    Root(SourceRoot),
    NonAuthority {
        directory: PathBuf,
        reason: SourceRootNonAuthority,
    },
}

/// Why the registry base itself cannot be trusted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistryBaseDefect {
    #[error("is not a real directory")]
    NotADirectory,
    #[error("is owned by uid {uid}; expected uid {expected}")]
    WrongOwner { uid: u32, expected: u32 },
    #[error("mode {mode:04o} is writable by group or others")]
    WritableByOthers { mode: u32 },
}

#[derive(Debug, thiserror::Error)]
pub enum SourceRootError {
    #[error("source root registry base {} {defect}", base.display())]
    UnsafeRegistryBase {
        base: PathBuf,
        defect: RegistryBaseDefect,
    },
    #[error("source root {name} already exists")]
    AlreadyExists { name: SourceRootName },
    #[error("source root {name} does not exist")]
    NotFound { name: SourceRootName },
    #[error("source root {name} is not an authority: {reason}")]
    NonAuthority {
        name: SourceRootName,
        reason: SourceRootNonAuthority,
    },
    #[error("source root I/O failed at {}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("source root database {} failed: {source}", path.display())]
    Database {
        path: PathBuf,
        source: Box<crate::Error>,
    },
}

fn io_error(path: &Path) -> impl FnOnce(io::Error) -> SourceRootError + '_ {
    move |source| SourceRootError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// The registry of source roots under one base directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRootRegistry {
    base: PathBuf,
    expected_owner: u32,
}

impl SourceRootRegistry {
    /// The host registry at [`DEFAULT_SOURCE_ROOTS_BASE`].
    pub fn host() -> Self {
        Self::new(DEFAULT_SOURCE_ROOTS_BASE)
    }

    /// A registry at `base`, owned by the effective user.
    pub fn new(base: impl Into<PathBuf>) -> Self {
        Self {
            base: base.into(),
            expected_owner: nix::unistd::geteuid().as_raw(),
        }
    }

    /// Require a different owner uid; lets tests produce ownership defects
    /// without privilege.
    #[cfg(test)]
    pub(super) fn with_expected_owner(mut self, uid: u32) -> Self {
        self.expected_owner = uid;
        self
    }

    pub fn base(&self) -> &Path {
        &self.base
    }

    /// The directory a root of this name occupies.
    pub fn root_dir(&self, name: &SourceRootName) -> PathBuf {
        self.base.join(name.as_str())
    }

    /// The runtime root a root of this name uses.
    pub fn runtime_root(&self, name: &SourceRootName) -> ConaryRuntimeRoot {
        let root = self.root_dir(name);
        let db_path = root.join(SOURCE_ROOT_DB_FILE);
        ConaryRuntimeRoot::new(root, db_path)
    }

    /// Create a root: a `0700` directory whose database holds the current
    /// schema and the pinned identity, committed in one transaction.
    ///
    /// The root is assembled in a staging directory and renamed into place
    /// without replacement, so it appears complete or not at all.
    pub fn create(&self, name: &SourceRootName) -> Result<SourceRoot, SourceRootError> {
        self.ensure_base()?;
        let target = self.root_dir(name);
        match std::fs::symlink_metadata(&target) {
            Ok(_) => return Err(SourceRootError::AlreadyExists { name: name.clone() }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error(&target)(error)),
        }

        let staging = tempfile::Builder::new()
            .prefix(STAGING_PREFIX)
            .tempdir_in(&self.base)
            .map_err(io_error(&self.base))?;
        std::fs::set_permissions(staging.path(), Permissions::from_mode(SOURCE_ROOT_DIR_MODE))
            .map_err(io_error(staging.path()))?;
        let staged_db = staging.path().join(SOURCE_ROOT_DB_FILE);
        initialize_pinned_database(&staged_db, name).map_err(|source| {
            SourceRootError::Database {
                path: staged_db.clone(),
                source: Box::new(source),
            }
        })?;
        std::fs::File::open(staging.path())
            .and_then(|dir| dir.sync_all())
            .map_err(io_error(staging.path()))?;

        let staging = staging.keep();
        let renamed = nix::fcntl::renameat2(
            nix::fcntl::AT_FDCWD,
            staging.as_path(),
            nix::fcntl::AT_FDCWD,
            target.as_path(),
            nix::fcntl::RenameFlags::RENAME_NOREPLACE,
        );
        if let Err(errno) = renamed {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(match errno {
                nix::errno::Errno::EEXIST => SourceRootError::AlreadyExists { name: name.clone() },
                errno => io_error(&target)(io::Error::from(errno)),
            });
        }
        std::fs::File::open(&self.base)
            .and_then(|dir| dir.sync_all())
            .map_err(io_error(&self.base))?;

        self.open(name)
    }

    /// Open an existing root, refusing any non-authority state.
    pub fn open(&self, name: &SourceRootName) -> Result<SourceRoot, SourceRootError> {
        if !self.validate_base()? {
            return Err(SourceRootError::NotFound { name: name.clone() });
        }
        let dir = self.root_dir(name);
        match std::fs::symlink_metadata(&dir) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(SourceRootError::NotFound { name: name.clone() });
            }
            Err(error) => return Err(io_error(&dir)(error)),
        }
        self.classify(name)?
            .map_err(|reason| SourceRootError::NonAuthority {
                name: name.clone(),
                reason,
            })
    }

    /// List every registry entry, sorted by directory name, as a root or a
    /// typed non-authority. A missing base lists nothing.
    pub fn list(&self) -> Result<Vec<SourceRootEntry>, SourceRootError> {
        if !self.validate_base()? {
            return Ok(Vec::new());
        }
        let mut names = std::fs::read_dir(&self.base)
            .map_err(io_error(&self.base))?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<io::Result<Vec<_>>>()
            .map_err(io_error(&self.base))?;
        names.sort();

        let mut entries = Vec::with_capacity(names.len());
        for file_name in names {
            let directory = self.base.join(&file_name);
            let classified = match SourceRootName::from_os_str(&file_name) {
                Ok(name) => self.classify(&name)?,
                Err(error) => Err(SourceRootNonAuthority::InvalidName(error)),
            };
            entries.push(match classified {
                Ok(root) => SourceRootEntry::Root(root),
                Err(reason) => SourceRootEntry::NonAuthority { directory, reason },
            });
        }
        Ok(entries)
    }

    fn classify(
        &self,
        name: &SourceRootName,
    ) -> Result<Result<SourceRoot, SourceRootNonAuthority>, SourceRootError> {
        let runtime_root = self.runtime_root(name);
        let dir = runtime_root.root();
        let metadata = std::fs::symlink_metadata(dir).map_err(io_error(dir))?;
        if let Some(defect) = root_directory_defect(
            metadata.file_type().is_dir(),
            metadata.uid(),
            metadata.mode(),
            self.expected_owner,
        ) {
            return Ok(Err(defect));
        }

        let db_path = runtime_root.db_path();
        match std::fs::symlink_metadata(db_path) {
            Ok(metadata) if metadata.file_type().is_file() => {}
            Ok(_) => return Ok(Err(SourceRootNonAuthority::DatabaseNotRegularFile)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Err(SourceRootNonAuthority::MissingDatabase));
            }
            Err(error) => return Err(io_error(db_path)(error)),
        }
        match inspect(db_path) {
            Ok(SchemaCompatibility::Current) => {}
            Ok(SchemaCompatibility::Fresh) => {
                return Ok(Err(SourceRootNonAuthority::UninitializedDatabase));
            }
            Ok(SchemaCompatibility::RebuildRequired { observed }) => {
                return Ok(Err(SourceRootNonAuthority::SchemaRebuildRequired {
                    observed,
                }));
            }
            Err(error) => {
                return Ok(Err(SourceRootNonAuthority::UnreadableDatabase {
                    error: error.to_string(),
                }));
            }
        }

        let pin = Connection::open_with_flags(
            db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(crate::Error::from)
        .and_then(|conn| read_pin(&conn));
        let pin = match pin {
            Ok(pin) => pin,
            Err(error) => {
                return Ok(Err(SourceRootNonAuthority::UnreadableDatabase {
                    error: error.to_string(),
                }));
            }
        };
        Ok(match pin {
            StoredPin::Missing => Err(SourceRootNonAuthority::MissingPin),
            StoredPin::Malformed { identity, error } => {
                Err(SourceRootNonAuthority::MalformedPin { identity, error })
            }
            StoredPin::Present {
                identity,
                created_at,
            } if identity == *name => Ok(SourceRoot {
                name: identity,
                pinned_at: created_at,
                runtime_root,
            }),
            StoredPin::Present { identity, .. } => {
                Err(SourceRootNonAuthority::PinMismatch { pinned: identity })
            }
        })
    }

    /// Create the base when absent, then validate it.
    fn ensure_base(&self) -> Result<(), SourceRootError> {
        match std::fs::symlink_metadata(&self.base) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                DirBuilder::new()
                    .recursive(true)
                    .mode(SOURCE_ROOTS_BASE_MODE)
                    .create(&self.base)
                    .map_err(io_error(&self.base))?;
                // The process umask must not narrow the documented mode.
                std::fs::set_permissions(
                    &self.base,
                    Permissions::from_mode(SOURCE_ROOTS_BASE_MODE),
                )
                .map_err(io_error(&self.base))?;
            }
            Err(error) => return Err(io_error(&self.base)(error)),
        }
        self.validate_base().map(|_| ())
    }

    /// Validate the base; `Ok(false)` means it does not exist.
    fn validate_base(&self) -> Result<bool, SourceRootError> {
        let metadata = match std::fs::symlink_metadata(&self.base) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(io_error(&self.base)(error)),
        };
        let defect = if !metadata.file_type().is_dir() {
            Some(RegistryBaseDefect::NotADirectory)
        } else if metadata.uid() != self.expected_owner {
            Some(RegistryBaseDefect::WrongOwner {
                uid: metadata.uid(),
                expected: self.expected_owner,
            })
        } else if metadata.mode() & 0o022 != 0 {
            Some(RegistryBaseDefect::WritableByOthers {
                mode: metadata.mode() & 0o7777,
            })
        } else {
            None
        };
        match defect {
            Some(defect) => Err(SourceRootError::UnsafeRegistryBase {
                base: self.base.clone(),
                defect,
            }),
            None => Ok(true),
        }
    }
}

/// The directory-level non-authority of a root entry, if any. Symlinks are
/// never followed: the caller passes `lstat` facts.
pub(super) fn root_directory_defect(
    is_dir: bool,
    uid: u32,
    mode: u32,
    expected_owner: u32,
) -> Option<SourceRootNonAuthority> {
    if !is_dir {
        return Some(SourceRootNonAuthority::NotADirectory);
    }
    if uid != expected_owner {
        return Some(SourceRootNonAuthority::WrongOwner {
            uid,
            expected: expected_owner,
        });
    }
    let mode = mode & 0o7777;
    (mode != SOURCE_ROOT_DIR_MODE).then_some(SourceRootNonAuthority::UnsafePermissions { mode })
}
