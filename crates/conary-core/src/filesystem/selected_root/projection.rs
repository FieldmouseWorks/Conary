// crates/conary-core/src/filesystem/selected_root/projection.rs
//! The unified projected selected-root resolver.
//!
//! A [`SelectedRootProjection`] overlays a transaction's introduced payload
//! nodes on the pre-transaction selected root. Resolution walks one component
//! at a time and consults, for each prefix, the transaction's tombstones,
//! then the overlay, then the on-disk selected root. Callers apply events in
//! transaction order through [`SelectedRootProjection::insert`] and
//! [`SelectedRootProjection::remove`]; nothing here mutates the filesystem.
//!
//! [`super::selected_root_executable`] is the empty projection: the on-disk
//! selected root with no introduced nodes and no tombstones.

use super::{
    MAX_SELECTED_ROOT_SYMLINK_DEPTH, components, root_relative_package_path, splice_symlink_target,
    validate_selected_root,
};
use crate::error::{Error, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// One node the projection materializes at a normalized absolute package path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectedNode {
    /// A regular file. `executable` is `mode & 0o111 != 0`.
    Regular { executable: bool },
    /// A symlink whose target is a package-path spelling. An absolute target
    /// is selected-root-relative, never host-relative.
    Symlink { target: String },
    /// A hardlink to another package path in the projection.
    Hardlink { target: String },
    /// An explicit directory node.
    Directory,
    /// A FIFO, socket, or device node. It exists but is never executable.
    Other,
}

/// The typed outcome of resolving one package path as an executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectedExecutable {
    /// The path resolves to a regular file with at least one execute bit.
    Executable { resolved: String },
    /// The path resolves to an existing node that cannot execute.
    NotExecutable { resolved: String },
    /// No component chain reaches a node in the projected state.
    Missing,
}

/// The pre-transaction selected root with a transaction's projected payload
/// overlaid on it.
///
/// Overlay keys and tombstones are normalized absolute package paths, the same
/// spelling the payload and removal records use. Resolution never resolves one
/// spelling through the other's domain: an overlay entry shadows the on-disk
/// selected root, and a tombstone hides on-disk or earlier-overlay entries.
#[derive(Debug, Clone)]
pub struct SelectedRootProjection<'a> {
    root: &'a Path,
    overlay: BTreeMap<String, ProjectedNode>,
    removed: BTreeSet<String>,
}

impl<'a> SelectedRootProjection<'a> {
    /// An empty projection over `root`: the on-disk selected root with no
    /// introduced nodes and no tombstones.
    pub fn new(root: &'a Path) -> Self {
        Self {
            root,
            overlay: BTreeMap::new(),
            removed: BTreeSet::new(),
        }
    }

    /// Materialize `node` at `path`, superseding any earlier tombstone there.
    ///
    /// A removed ancestor becomes an explicit directory, because inserting a
    /// descendant recreates the ancestor path. An ancestor that is not removed
    /// is left to the on-disk root (or to an implied-parent walk), so a payload
    /// written through an existing root symlink never shadows it.
    pub fn insert(&mut self, path: &str, node: ProjectedNode) -> Result<()> {
        let key = normalize_key(path)?;
        for ancestor in ancestor_keys(&key) {
            if self.overlay.contains_key(&ancestor) {
                continue;
            }
            if self.removed.remove(&ancestor) {
                self.overlay.insert(ancestor, ProjectedNode::Directory);
            }
        }
        self.removed.remove(&key);
        self.overlay.insert(key, node);
        Ok(())
    }

    /// Tombstone `path` so it is unreachable even when an earlier overlay or
    /// the on-disk selected root has it.
    pub fn remove(&mut self, path: &str) -> Result<()> {
        let key = normalize_key(path)?;
        self.overlay.remove(&key);
        self.removed.insert(key);
        Ok(())
    }

    /// Resolve `path` (an absolute package spelling) against the projection and
    /// classify it as an executable regular file or not.
    ///
    /// Symlinks are followed component by component; an absolute target is
    /// selected-root-relative, and a relative target resolves against the
    /// link's parent. Symlink-depth overflow and hardlink cycles are typed
    /// errors, never `Missing`.
    pub fn resolve_executable(&self, path: &str) -> Result<ProjectedExecutable> {
        validate_selected_root(self.root)?;
        let mut current = root_relative_package_path(path)?;
        let mut followed_hardlinks = BTreeSet::new();
        loop {
            match self.walk(&current, path)? {
                WalkedNode::Missing => return Ok(ProjectedExecutable::Missing),
                WalkedNode::Regular {
                    resolved,
                    executable,
                } => {
                    return Ok(if executable {
                        ProjectedExecutable::Executable { resolved }
                    } else {
                        ProjectedExecutable::NotExecutable { resolved }
                    });
                }
                WalkedNode::Directory { resolved } | WalkedNode::Other { resolved } => {
                    return Ok(ProjectedExecutable::NotExecutable { resolved });
                }
                WalkedNode::Hardlink { resolved, target } => {
                    if !followed_hardlinks.insert(resolved) {
                        return Err(Error::PathTraversal(format!(
                            "package path {path} has a hardlink cycle in the selected-root projection"
                        )));
                    }
                    current = root_relative_package_path(&target)?;
                }
            }
        }
    }

    /// Walk one package spelling, consulting the tombstone set, the overlay,
    /// and the on-disk root for every component prefix.
    fn walk(&self, relative: &Path, package_path: &str) -> Result<WalkedNode> {
        let mut pending = components(relative)?;
        let mut resolved = PathBuf::new();
        let mut symlink_depth = 0usize;

        while let Some(component) = pending.pop_front() {
            let candidate_relative = resolved.join(&component);
            let prefix = absolute_package_path(&candidate_relative)?;
            let is_final = pending.is_empty();

            // A tombstone hides everything behind it. A later overlay entry at
            // the same key supersedes the tombstone, so the overlay guard only
            // applies to a key the overlay does not itself provide.
            if self.removed.contains(&prefix) && !self.overlay.contains_key(&prefix) {
                return Ok(WalkedNode::Missing);
            }

            if let Some(node) = self.overlay.get(&prefix) {
                match node {
                    ProjectedNode::Symlink { target } => {
                        symlink_depth += 1;
                        check_symlink_depth(symlink_depth, package_path)?;
                        pending = splice_symlink_target(
                            &resolved,
                            Path::new(target.as_str()),
                            package_path,
                            &candidate_relative,
                            pending,
                        )?;
                        resolved.clear();
                        continue;
                    }
                    ProjectedNode::Directory => {
                        if is_final {
                            return Ok(WalkedNode::Directory { resolved: prefix });
                        }
                        resolved.push(component);
                        continue;
                    }
                    ProjectedNode::Regular { executable } => {
                        if is_final {
                            return Ok(WalkedNode::Regular {
                                resolved: prefix,
                                executable: *executable,
                            });
                        }
                        return Ok(WalkedNode::Missing);
                    }
                    ProjectedNode::Hardlink { target } => {
                        if is_final {
                            return Ok(WalkedNode::Hardlink {
                                resolved: prefix,
                                target: target.clone(),
                            });
                        }
                        return Ok(WalkedNode::Missing);
                    }
                    ProjectedNode::Other => {
                        if is_final {
                            return Ok(WalkedNode::Other { resolved: prefix });
                        }
                        return Ok(WalkedNode::Missing);
                    }
                }
            }

            let candidate = self.root.join(&candidate_relative);
            let metadata = match fs::symlink_metadata(&candidate) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    // No on-disk node: a deeper overlay entry materializes this
                    // prefix as an implied directory.
                    if self.has_overlay_descendant(&prefix) {
                        if is_final {
                            return Ok(WalkedNode::Directory { resolved: prefix });
                        }
                        resolved.push(component);
                        continue;
                    }
                    return Ok(WalkedNode::Missing);
                }
                Err(error) => return Err(Error::Io(error)),
            };
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                symlink_depth += 1;
                check_symlink_depth(symlink_depth, package_path)?;
                let target = fs::read_link(&candidate).map_err(Error::Io)?;
                pending = splice_symlink_target(
                    &resolved,
                    &target,
                    package_path,
                    &candidate_relative,
                    pending,
                )?;
                resolved.clear();
                continue;
            }
            if file_type.is_dir() {
                if is_final {
                    return Ok(WalkedNode::Directory { resolved: prefix });
                }
                resolved.push(component);
                continue;
            }
            if is_final {
                if file_type.is_file() {
                    return Ok(WalkedNode::Regular {
                        resolved: prefix,
                        executable: metadata.permissions().mode() & 0o111 != 0,
                    });
                }
                return Ok(WalkedNode::Other { resolved: prefix });
            }
            // A non-directory cannot have children.
            return Ok(WalkedNode::Missing);
        }

        // The walk exhausted its components on the selected root itself.
        Ok(WalkedNode::Missing)
    }

    /// Whether any overlay key is a proper descendant of `prefix`.
    fn has_overlay_descendant(&self, prefix: &str) -> bool {
        let range_start = format!("{prefix}/");
        self.overlay
            .range::<str, _>((
                std::ops::Bound::Included(range_start.as_str()),
                std::ops::Bound::Unbounded,
            ))
            .next()
            .is_some_and(|(key, _)| key.starts_with(range_start.as_str()))
    }
}

/// The result of walking one package spelling through the projection.
#[derive(Debug, Clone, PartialEq, Eq)]
enum WalkedNode {
    Missing,
    Regular { resolved: String, executable: bool },
    Directory { resolved: String },
    Other { resolved: String },
    Hardlink { resolved: String, target: String },
}

/// Normalize one package spelling to the absolute key form the projection uses.
fn normalize_key(path: &str) -> Result<String> {
    let relative = root_relative_package_path(path)?;
    absolute_package_path(&relative)
}

/// Render a root-relative path as its normalized absolute package spelling.
fn absolute_package_path(relative: &Path) -> Result<String> {
    let text = relative.to_str().ok_or_else(|| {
        Error::InvalidPath(format!(
            "projected selected-root path {} is not UTF-8",
            relative.display()
        ))
    })?;
    if text.is_empty() {
        return Err(Error::InvalidPath(
            "projected selected-root path is empty".to_string(),
        ));
    }
    Ok(format!("/{text}"))
}

/// Every proper ancestor of one absolute key, shallowest first.
fn ancestor_keys(key: &str) -> Vec<String> {
    let mut ancestors = Vec::new();
    let mut current = String::new();
    let mut components = key.trim_start_matches('/').split('/').peekable();
    while let Some(component) = components.next() {
        if components.peek().is_none() {
            break;
        }
        current.push('/');
        current.push_str(component);
        ancestors.push(current.clone());
    }
    ancestors
}

/// Bound symlink redirects the same way the on-disk resolver does; overflow is
/// a resolution error, never unavailability.
fn check_symlink_depth(depth: usize, package_path: &str) -> Result<()> {
    if depth > MAX_SELECTED_ROOT_SYMLINK_DEPTH {
        return Err(Error::PathTraversal(format!(
            "package path {package_path} exceeds {MAX_SELECTED_ROOT_SYMLINK_DEPTH} projected selected-root symlinks"
        )));
    }
    Ok(())
}

#[cfg(test)]
#[path = "projection/tests.rs"]
mod tests;
