// apps/conary/src/commands/live_root/path.rs

//! Lexical package-path resolution for live and selected roots.

use anyhow::{Result, bail};
use conary_core::filesystem::SelectedRootProjection;
use std::path::{Component, Path, PathBuf};

pub(crate) fn target_path(root: &Path, package_path: &str) -> Result<PathBuf> {
    let relative = package_path.strip_prefix('/').unwrap_or(package_path);
    let relative_path = Path::new(relative);
    let mut has_path_below_root = false;
    for component in relative_path.components() {
        match component {
            Component::Normal(_) => has_path_below_root = true,
            Component::CurDir => {
                bail!(
                    "package path {package_path} must name a file or directory below the target root"
                );
            }
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!("package path {package_path} escapes the target root");
            }
        }
    }
    if !has_path_below_root {
        bail!("package path {package_path} must name a file or directory below the target root");
    }
    Ok(root.join(relative_path))
}

pub(super) fn selected_root_target_path(root: &Path, package_path: &str) -> Result<PathBuf> {
    let projection = SelectedRootProjection::new(root);
    projection_target_path(&projection, package_path)
}

/// The on-disk target for one package path, resolved through `projection` in
/// the same effective-path domain execution's aliases use.
pub(super) fn projection_target_path(
    projection: &SelectedRootProjection<'_>,
    package_path: &str,
) -> Result<PathBuf> {
    let effective = projection.effective_package_path(package_path)?;
    target_path(projection.root(), &effective)
}
