// crates/conary-core/src/runtime_root.rs

use std::path::{Path, PathBuf};

const DEFAULT_RUNTIME_ROOT: &str = "/conary";
const DEFAULT_DB_PATH: &str = "/var/lib/conary/conary.db";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConaryRuntimeRoot {
    root: PathBuf,
    db_path: PathBuf,
}

impl Default for ConaryRuntimeRoot {
    fn default() -> Self {
        Self {
            root: PathBuf::from(DEFAULT_RUNTIME_ROOT),
            db_path: PathBuf::from(DEFAULT_DB_PATH),
        }
    }
}

impl ConaryRuntimeRoot {
    pub fn new(root: impl Into<PathBuf>, db_path: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            db_path: db_path.into(),
        }
    }

    pub fn for_test_root(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            db_path: root.join("conary.db"),
            root,
        }
    }

    pub fn from_db_path(db_path: impl Into<PathBuf>) -> Self {
        let db_path = db_path.into();
        if db_path == Path::new(DEFAULT_DB_PATH) {
            return Self::new(DEFAULT_RUNTIME_ROOT, db_path);
        }

        let root = db_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        Self::new(root, db_path)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    pub fn objects_dir(&self) -> PathBuf {
        self.root.join("objects")
    }

    pub fn generations_dir(&self) -> PathBuf {
        self.root.join("generations")
    }

    pub fn generation_path(&self, number: i64) -> PathBuf {
        self.generations_dir().join(number.to_string())
    }

    pub fn current_link(&self) -> PathBuf {
        self.root.join("current")
    }

    pub fn mount_dir(&self) -> PathBuf {
        self.root.join("mnt")
    }

    pub fn etc_state_dir(&self) -> PathBuf {
        self.root.join("etc-state")
    }

    pub fn gc_roots_dir(&self) -> PathBuf {
        self.root.join("gc-roots")
    }

    /// Repository and generation-metadata trust keys for this runtime root.
    ///
    /// The default host runtime root keeps its keyring at
    /// `/var/lib/conary/keys` (honouring `CONARY_DB_DIR` exactly as
    /// [`crate::db::paths::keyring_dir`] does). Every other runtime root,
    /// including each source root, owns a disjoint `<root>/keys`.
    pub fn keys_dir(&self) -> PathBuf {
        if self.is_default_host() {
            crate::db::paths::keyring_dir(DEFAULT_DB_PATH)
        } else {
            self.root.join("keys")
        }
    }

    /// Whether this is the default host runtime root (`/conary` with the
    /// default host database).
    pub fn is_default_host(&self) -> bool {
        self.root == Path::new(DEFAULT_RUNTIME_ROOT) && self.db_path == Path::new(DEFAULT_DB_PATH)
    }
}

#[cfg(test)]
mod tests {
    use super::ConaryRuntimeRoot;
    use std::path::{Path, PathBuf};

    #[test]
    fn defaults_keep_boot_visible_generation_state_under_conary() {
        let root = ConaryRuntimeRoot::default();

        assert_eq!(root.root(), Path::new("/conary"));
        assert_eq!(root.db_path(), Path::new("/var/lib/conary/conary.db"));
        assert_eq!(root.objects_dir(), Path::new("/conary/objects"));
        assert_eq!(root.generations_dir(), Path::new("/conary/generations"));
        assert_eq!(root.current_link(), Path::new("/conary/current"));
        assert_eq!(root.mount_dir(), Path::new("/conary/mnt"));
        assert_eq!(root.etc_state_dir(), Path::new("/conary/etc-state"));
        assert_eq!(root.gc_roots_dir(), Path::new("/conary/gc-roots"));
        assert!(root.is_default_host());
    }

    #[test]
    fn keys_dir_keeps_host_keyring_and_isolates_other_roots() {
        let host = ConaryRuntimeRoot::default();
        assert_eq!(
            host.keys_dir(),
            crate::db::paths::keyring_dir("/var/lib/conary/conary.db")
        );
        assert_eq!(
            ConaryRuntimeRoot::from_db_path("/var/lib/conary/conary.db").keys_dir(),
            host.keys_dir()
        );

        let other = ConaryRuntimeRoot::new(
            "/var/lib/conary/roots/arch",
            "/var/lib/conary/roots/arch/conary.db",
        );
        assert!(!other.is_default_host());
        assert_eq!(
            other.keys_dir(),
            Path::new("/var/lib/conary/roots/arch/keys")
        );

        // A non-default database under the boot runtime root is not the host.
        let shared_root = ConaryRuntimeRoot::new("/conary", "/tmp/elsewhere/conary.db");
        assert!(!shared_root.is_default_host());
        assert_eq!(shared_root.keys_dir(), Path::new("/conary/keys"));
    }

    #[test]
    fn test_roots_can_use_temp_runtime_state_without_changing_db_name() {
        let root = ConaryRuntimeRoot::for_test_root("/tmp/conary-test");

        assert_eq!(root.root(), Path::new("/tmp/conary-test"));
        assert_eq!(root.db_path(), Path::new("/tmp/conary-test/conary.db"));
        assert_eq!(
            root.generation_path(7),
            Path::new("/tmp/conary-test/generations/7")
        );
    }

    #[test]
    fn default_db_path_uses_conary_runtime_root() {
        let root = ConaryRuntimeRoot::from_db_path(PathBuf::from("/var/lib/conary/conary.db"));

        assert_eq!(root.root(), Path::new("/conary"));
        assert_eq!(root.db_path(), Path::new("/var/lib/conary/conary.db"));
        assert_eq!(root.generations_dir(), Path::new("/conary/generations"));
    }

    #[test]
    fn non_default_db_paths_remain_self_contained_for_tests() {
        let root = ConaryRuntimeRoot::from_db_path(PathBuf::from("/tmp/conary-test/conary.db"));

        assert_eq!(root.root(), Path::new("/tmp/conary-test"));
        assert_eq!(root.db_path(), Path::new("/tmp/conary-test/conary.db"));
        assert_eq!(root.objects_dir(), Path::new("/tmp/conary-test/objects"));
    }
}
