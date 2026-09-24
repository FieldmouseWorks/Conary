// apps/conary/src/commands/install/ccs_hook_interpreter.rs
//! Transaction-ordered availability of CCS hook interpreters.

use anyhow::Context;
use conary_core::db::models::{FileEntry, Trove};
use conary_core::filesystem::selected_root::{
    MAX_SELECTED_ROOT_SYMLINK_DEPTH, selected_root_effective_package_path, selected_root_executable,
};
use conary_core::packages::payload::PackagePayloadFile;
use conary_core::payload::PayloadNodeKind;
use conary_core::repository::dependency_model::{ProvidedCapability, RepositoryCapabilityKind};
use conary_core::transaction::PackageRelationRemoval;
use rusqlite::Connection;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

/// One payload path an element introduces, with the node it will materialize.
#[derive(Debug, Clone, PartialEq, Eq)]
struct IntroducedNode {
    path: String,
    kind: IntroducedNodeKind,
}

impl IntroducedNode {
    /// Classify one payload file entry.
    ///
    /// `Executable` requires a regular node with at least one execute bit.
    /// A symlink retains its target so the ledger can follow projected links;
    /// a hardlink retains the payload path whose inode it shares; every other
    /// node kind is `NonExecutable`.
    fn from_payload_file(file: &PackagePayloadFile) -> Self {
        let kind = match &file.node.kind {
            PayloadNodeKind::Regular { .. } if file.node.mode & 0o111 != 0 => {
                IntroducedNodeKind::Executable
            }
            PayloadNodeKind::Symlink { target } => IntroducedNodeKind::Symlink {
                target: target.clone(),
            },
            PayloadNodeKind::Hardlink { target, .. } => IntroducedNodeKind::Hardlink {
                target: target.clone(),
            },
            _ => IntroducedNodeKind::NonExecutable,
        };
        Self {
            path: file.path.clone(),
            kind,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum IntroducedNodeKind {
    Executable,
    NonExecutable,
    Symlink {
        target: String,
    },
    /// Shares the inode of another payload path in the same transaction.
    Hardlink {
        target: String,
    },
}

/// One transaction element's payload boundary and post-install interpreter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ElementPlan {
    package: String,
    version: String,
    removed_paths: Vec<String>,
    introduced_nodes: Vec<IntroducedNode>,
    declared_file_capabilities: Vec<String>,
    post_install_interpreter: Option<String>,
}

/// Build one element plan, resolving the removed paths of the installed state
/// it replaces. Declared `File` capabilities are recorded but remain claims,
/// never payload authority.
#[allow(clippy::too_many_arguments)]
pub(super) fn element_plan(
    conn: &Connection,
    package: &str,
    version: &str,
    old_trove: Option<&Trove>,
    relation_removals: &[PackageRelationRemoval],
    extracted_files: &[PackagePayloadFile],
    provides: &[ProvidedCapability],
    post_install_interpreter: Option<String>,
) -> anyhow::Result<ElementPlan> {
    let mut removed_paths = Vec::new();
    if let Some(trove_id) = old_trove.and_then(|trove| trove.id) {
        removed_paths.extend(
            FileEntry::find_by_trove(conn, trove_id)?
                .into_iter()
                .map(|file| file.path),
        );
    }
    for removal in relation_removals {
        removed_paths.extend(
            FileEntry::find_by_trove(conn, removal.trove_id)?
                .into_iter()
                .map(|file| file.path),
        );
    }
    Ok(ElementPlan {
        package: package.to_string(),
        version: version.to_string(),
        removed_paths,
        introduced_nodes: extracted_files
            .iter()
            .map(IntroducedNode::from_payload_file)
            .collect(),
        declared_file_capabilities: provides
            .iter()
            .filter(|capability| capability.kind == RepositoryCapabilityKind::File)
            .map(|capability| capability.name.clone())
            .collect(),
        post_install_interpreter,
    })
}

/// One removal-only element: every path the removed troves own leaves the
/// projected state before later elements are recorded.
pub(super) fn removal_element_plan(
    conn: &Connection,
    troves: &[Trove],
) -> anyhow::Result<ElementPlan> {
    let mut removed_paths = Vec::new();
    for trove_id in troves.iter().filter_map(|trove| trove.id) {
        removed_paths.extend(
            FileEntry::find_by_trove(conn, trove_id)?
                .into_iter()
                .map(|file| file.path),
        );
    }
    Ok(ElementPlan {
        package: String::new(),
        version: String::new(),
        removed_paths,
        introduced_nodes: Vec::new(),
        declared_file_capabilities: Vec::new(),
        post_install_interpreter: None,
    })
}

/// Record every element's payload boundary, then require each element's
/// post-install interpreter. Both passes complete before the caller mutates.
pub(super) fn preflight_post_install_interpreters(
    root: &Path,
    elements: &[ElementPlan],
) -> anyhow::Result<()> {
    let mut ledger = HookInterpreterLedger::new(root);
    for element in elements {
        ledger.apply_element(
            element.removed_paths.iter().cloned(),
            element.introduced_nodes.iter().cloned(),
            element.declared_file_capabilities.iter().cloned(),
        )?;
    }
    for element in elements {
        if let Some(interpreter) = element.post_install_interpreter.as_deref() {
            ledger.require(
                &element.package,
                &element.version,
                HookPhase::PostInstall,
                interpreter,
            )?;
        }
    }
    Ok(())
}

/// Transaction-ordered availability of CCS hook interpreters in one selected root.
struct HookInterpreterLedger {
    root: PathBuf,
    /// Effective absolute path -> payload node the transaction will materialize.
    introduced: BTreeMap<String, IntroducedNodeKind>,
    /// Effective absolute paths declared as `File` capabilities. A declaration
    /// is a claim, not materialized payload authority.
    declared_file_capabilities: BTreeSet<String>,
    /// Effective absolute paths whose final provider an earlier element removed.
    removed: BTreeSet<String>,
}

impl HookInterpreterLedger {
    fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            introduced: BTreeMap::new(),
            declared_file_capabilities: BTreeSet::new(),
            removed: BTreeSet::new(),
        }
    }

    /// Record one element's payload boundary: paths it removes (old-only
    /// paths of an upgrade/removal), payload nodes, and declared `File`
    /// capabilities it installs.
    fn apply_element(
        &mut self,
        removed_paths: impl IntoIterator<Item = String>,
        introduced_nodes: impl IntoIterator<Item = IntroducedNode>,
        declared_file_capabilities: impl IntoIterator<Item = String>,
    ) -> anyhow::Result<()> {
        for path in removed_paths {
            let Some(path) = self.effective_path(&path)? else {
                continue;
            };
            self.introduced.remove(&path);
            self.removed.insert(path);
        }
        for node in introduced_nodes {
            let Some(path) = self.effective_path(&node.path)? else {
                continue;
            };
            self.removed.remove(&path);
            self.introduced.insert(path, node.kind);
        }
        for path in declared_file_capabilities {
            let Some(path) = self.effective_path(&path)? else {
                continue;
            };
            self.declared_file_capabilities.insert(path);
        }
        Ok(())
    }

    /// Require `interpreter` to be available at this point in the transaction.
    ///
    /// Unavailability is the typed [`CcsHookInterpreterUnavailable`]; a
    /// selected-root resolution failure (for example a symlink loop) is
    /// propagated rather than reported as a missing interpreter.
    fn require(
        &self,
        package: &str,
        version: &str,
        phase: HookPhase,
        interpreter: &str,
    ) -> anyhow::Result<()> {
        let available = self.available(interpreter).with_context(|| {
            format!(
                "failed to resolve {phase} interpreter {interpreter} for {package} {version} in the selected root"
            )
        })?;
        if available {
            return Ok(());
        }
        Err(CcsHookInterpreterUnavailable {
            package: package.to_string(),
            version: version.to_string(),
            phase,
            interpreter: interpreter.to_string(),
        }
        .into())
    }

    /// Resolve `interpreter` against the projected state first and the
    /// selected root second.
    fn available(&self, interpreter: &str) -> anyhow::Result<bool> {
        let Some(path) = self.effective_path(interpreter)? else {
            return Ok(false);
        };
        self.projected_available(&path, 0)
    }

    fn projected_available(&self, path: &str, depth: usize) -> anyhow::Result<bool> {
        // A declared `File` capability only authorizes when the projected
        // payload actually materializes an executable node at that path.
        if self.declared_file_capabilities.contains(path) && self.introduced_executable(path)? {
            return Ok(true);
        }
        match self.introduced.get(path) {
            Some(IntroducedNodeKind::Executable) => Ok(true),
            Some(IntroducedNodeKind::NonExecutable) => Ok(false),
            Some(IntroducedNodeKind::Hardlink { .. }) => self.introduced_executable(path),
            Some(IntroducedNodeKind::Symlink { target }) => {
                if depth >= MAX_SELECTED_ROOT_SYMLINK_DEPTH {
                    anyhow::bail!(
                        "package path {path} exceeds {MAX_SELECTED_ROOT_SYMLINK_DEPTH} projected selected-root symlinks"
                    );
                }
                let Some(target_path) = self.effective_link_target(path, target)? else {
                    return Ok(false);
                };
                if self.introduced.contains_key(&target_path) || self.removed.contains(&target_path)
                {
                    self.projected_available(&target_path, depth + 1)
                } else {
                    Ok(selected_root_executable(&self.root, &target_path)
                        .with_context(|| {
                            format!("failed to resolve projected symlink target {target_path}")
                        })?
                        .is_some())
                }
            }
            None if self.removed.contains(path) => Ok(false),
            None => Ok(selected_root_executable(&self.root, path)
                .with_context(|| format!("failed to inspect selected-root path {path}"))?
                .is_some()),
        }
    }

    /// Whether the projected payload materializes an executable regular node at
    /// `path`. A hardlink shares its target's inode, so it is executable exactly
    /// when its target payload node is; it never resolves into the existing root.
    fn introduced_executable(&self, path: &str) -> anyhow::Result<bool> {
        match self.introduced.get(path) {
            Some(IntroducedNodeKind::Executable) => Ok(true),
            Some(IntroducedNodeKind::Hardlink { target }) => {
                let Some(target) = self.effective_path(target)? else {
                    return Ok(false);
                };
                Ok(matches!(
                    self.introduced.get(&target),
                    Some(IntroducedNodeKind::Executable)
                ))
            }
            _ => Ok(false),
        }
    }

    /// Resolve one package spelling to the selected-root effective path before
    /// any ledger comparison. Payload normalization uses this same authority.
    fn effective_path(&self, path: &str) -> anyhow::Result<Option<String>> {
        let Some(normalized) = normalized_absolute_path(path) else {
            return Ok(None);
        };
        let effective = selected_root_effective_package_path(&self.root, &normalized)
            .with_context(|| format!("failed to resolve selected-root path {path}"))?;
        Ok(Some(effective))
    }

    /// Follow a projected symlink target through the selected-root alias
    /// authority so both sides of the next comparison use one spelling.
    fn effective_link_target(&self, link: &str, target: &str) -> anyhow::Result<Option<String>> {
        let Some(joined) = join_symlink_target(link, target) else {
            return Ok(None);
        };
        let Some(normalized) = normalized_absolute_path(&joined) else {
            return Ok(None);
        };
        let effective = selected_root_effective_package_path(&self.root, &normalized)
            .with_context(|| format!("failed to resolve symlink target {joined} for {link}"))?;
        Ok(Some(effective))
    }
}

/// Lexically resolve a symlink target against the link's effective path.
/// `None` when the target escapes the selected root or is empty.
fn join_symlink_target(link: &str, target: &str) -> Option<String> {
    let mut components: Vec<String> = if target.starts_with('/') {
        Vec::new()
    } else {
        Path::new(link)
            .parent()
            .unwrap_or_else(|| Path::new("/"))
            .components()
            .filter_map(normal_component)
            .collect()
    };
    for component in Path::new(target).components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(part) => components.push(part.to_string_lossy().into_owned()),
            Component::ParentDir => {
                components.pop()?;
            }
            Component::Prefix(_) => return None,
        }
    }
    if components.is_empty() {
        return None;
    }
    Some(format!("/{}", components.join("/")))
}

fn normal_component(component: Component<'_>) -> Option<String> {
    match component {
        Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
        _ => None,
    }
}

/// Normalize one package path spelling to the absolute form used as ledger
/// keys. Manifest parsing already validated lifecycle interpreters with
/// `sanitize_path`; payload paths and `File` capabilities are absolute package
/// spellings. A spelling that authority rejects cannot name an executable
/// provider, so it normalizes to `None`.
fn normalized_absolute_path(path: &str) -> Option<String> {
    conary_core::filesystem::path::sanitize_path(path)
        .ok()
        .map(|relative| format!("/{}", relative.display()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HookPhase {
    PostInstall,
}

impl std::fmt::Display for HookPhase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::PostInstall => "post-install",
        })
    }
}

#[derive(Debug, thiserror::Error)]
#[error(
    "{phase} hook for {package} {version} requires interpreter {interpreter}, but no installed or planned package provides it in the selected root; install a package that provides {interpreter} (the hook declares it as a pre-install requirement) or enroll a repository that supplies one"
)]
pub(super) struct CcsHookInterpreterUnavailable {
    pub package: String,
    pub version: String,
    pub phase: HookPhase,
    pub interpreter: String,
}

#[cfg(test)]
mod tests;
