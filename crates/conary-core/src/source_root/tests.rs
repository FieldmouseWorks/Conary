// crates/conary-core/src/source_root/tests.rs

use super::*;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

mod classification;
mod name_grammar;
mod pin_storage;
mod registry_lifecycle;

fn name(value: &str) -> SourceRootName {
    SourceRootName::parse(value).unwrap()
}

/// A registry whose base does not exist yet.
fn fresh_registry() -> (tempfile::TempDir, SourceRootRegistry) {
    let temp = tempfile::tempdir().unwrap();
    let registry = SourceRootRegistry::new(temp.path().join("roots"));
    (temp, registry)
}

fn mode_of(path: &Path) -> u32 {
    std::fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o7777
}

fn set_mode(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}
