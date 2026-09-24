// apps/conary/src/commands/install/ccs_hook_interpreter.rs
//! Transaction-ordered availability of CCS hook interpreters.

use anyhow::Context;
use conary_core::ccs::manifest::Hooks;
use conary_core::db::models::{PackagePayloadOwnership, PayloadClaim, Trove};
use conary_core::filesystem::selected_root::MAX_SELECTED_ROOT_SYMLINK_DEPTH;
use conary_core::packages::payload::PackagePayloadFile;
use conary_core::payload::PayloadNodeKind;
use conary_core::repository::dependency_model::{ProvidedCapability, RepositoryCapabilityKind};
use conary_core::transaction::PackageRelationRemoval;
use rusqlite::Connection;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::os::unix::fs::PermissionsExt;
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
            PayloadNodeKind::Directory => IntroducedNodeKind::Directory,
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
    /// An explicit payload directory node.
    Directory,
    /// A missing parent the payload's materialization creates for an
    /// introduced descendant. It is a directory only where the selected root
    /// has no node at that path; an existing root node (for example a
    /// `/bin -> usr/bin` symlink) is written through, never replaced.
    ImpliedDirectory,
    Symlink {
        target: String,
    },
    /// Shares the inode of another payload path in the same transaction.
    Hardlink {
        target: String,
    },
}

/// The projected-state outcome of resolving one package path.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Resolved {
    /// No component chain reaches a node in the projected state.
    Missing,
    /// The final component is a payload node the transaction materializes.
    Introduced(IntroducedNodeKind),
    /// The final component is a pre-transaction file in the selected root.
    RootFile { executable: bool },
}

/// One transaction element's payload boundary and hook interpreters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ElementPlan {
    package: String,
    version: String,
    /// Installed troves this element removes (old version, relation
    /// removals, or restore removals). Their paths are resolved claim-aware at
    /// preflight, against every trove the whole transaction removes.
    removed_trove_ids: Vec<i64>,
    /// Paths this element removes that are already resolved.
    removed_paths: Vec<String>,
    introduced_nodes: Vec<IntroducedNode>,
    declared_file_capabilities: Vec<String>,
    hook_interpreters: Vec<HookInterpreter>,
}

/// Build one element plan, resolving the removed paths of the installed state
/// it replaces. Declared `File` capabilities are recorded but remain claims,
/// never payload authority.
#[allow(clippy::too_many_arguments)]
pub(super) fn element_plan(
    package: &str,
    version: &str,
    old_trove: Option<&Trove>,
    relation_removals: &[PackageRelationRemoval],
    extracted_files: &[PackagePayloadFile],
    provides: &[ProvidedCapability],
    hook_interpreters: Vec<HookInterpreter>,
) -> anyhow::Result<ElementPlan> {
    let removed_trove_ids = old_trove
        .and_then(|trove| trove.id)
        .into_iter()
        .chain(relation_removals.iter().map(|removal| removal.trove_id))
        .collect();
    Ok(ElementPlan {
        package: package.to_string(),
        version: version.to_string(),
        removed_trove_ids,
        removed_paths: Vec::new(),
        introduced_nodes: extracted_files
            .iter()
            .map(IntroducedNode::from_payload_file)
            .collect(),
        declared_file_capabilities: provides
            .iter()
            .filter(|capability| capability.kind == RepositoryCapabilityKind::File)
            .map(|capability| capability.name.clone())
            .collect(),
        hook_interpreters,
    })
}

/// One removal-only element: every path the removed troves own leaves the
/// projected state before later elements are recorded.
pub(super) fn removal_element_plan(troves: &[Trove]) -> ElementPlan {
    ElementPlan {
        package: String::new(),
        version: String::new(),
        removed_trove_ids: troves.iter().filter_map(|trove| trove.id).collect(),
        removed_paths: Vec::new(),
        introduced_nodes: Vec::new(),
        declared_file_capabilities: Vec::new(),
        hook_interpreters: Vec::new(),
    }
}

/// Record every element's payload boundary, then require every element's hook
/// interpreters, each against the transaction's final projected state. Both
/// passes complete before the caller mutates.
pub(super) fn preflight_hook_interpreters(
    conn: &Connection,
    root: &Path,
    elements: &[ElementPlan],
) -> anyhow::Result<()> {
    let transaction_removed = elements
        .iter()
        .flat_map(|element| element.removed_trove_ids.iter().copied())
        .collect::<BTreeSet<_>>();
    let claims = if transaction_removed.is_empty() {
        None
    } else {
        Some(PayloadClaim::index_all(conn)?)
    };
    let mut ledger = HookInterpreterLedger::new(root);
    for element in elements {
        let mut removed_paths = element.removed_paths.clone();
        if let Some(claims) = claims.as_ref() {
            removed_paths.extend(PackagePayloadOwnership::released_paths(
                conn,
                claims,
                &element.removed_trove_ids,
                &transaction_removed,
            )?);
        }
        ledger.apply_element(
            removed_paths,
            element.introduced_nodes.iter().cloned(),
            element.declared_file_capabilities.iter().cloned(),
        )?;
    }
    for element in elements {
        for hook in &element.hook_interpreters {
            ledger.require(
                &element.package,
                &element.version,
                hook.phase,
                &hook.interpreter,
            )?;
        }
    }
    Ok(())
}

/// Transaction-ordered availability of CCS hook interpreters in one selected
/// root.
///
/// Ledger keys are the literal normalized absolute package spellings payload
/// and removal records use. Alias resolution happens in
/// [`HookInterpreterLedger::resolve_projected`], never while recording a key,
/// so a transaction that removes an ancestor symlink cannot be redirected
/// through the pre-transaction root.
struct HookInterpreterLedger {
    root: PathBuf,
    /// Normalized absolute package path -> payload node the transaction will
    /// materialize.
    introduced: BTreeMap<String, IntroducedNodeKind>,
    /// Normalized absolute paths declared as `File` capabilities. A
    /// declaration is a claim, not materialized payload authority.
    declared_file_capabilities: BTreeSet<String>,
    /// Normalized absolute paths whose final provider an earlier element
    /// removed.
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
    ///
    /// Every key keeps its literal package spelling; only syntax is
    /// normalized. Resolving a recorded path through the pre-transaction root
    /// here is the defect this ledger exists to avoid.
    fn apply_element(
        &mut self,
        removed_paths: impl IntoIterator<Item = String>,
        introduced_nodes: impl IntoIterator<Item = IntroducedNode>,
        declared_file_capabilities: impl IntoIterator<Item = String>,
    ) -> anyhow::Result<()> {
        for path in removed_paths {
            let Some(path) = normalized_absolute_path(&path) else {
                continue;
            };
            self.introduced.remove(&path);
            self.removed.insert(path);
        }
        for node in introduced_nodes {
            let Some(path) = normalized_absolute_path(&node.path) else {
                continue;
            };
            self.record_implied_parents(&path);
            self.removed.remove(&path);
            self.introduced.insert(path, node.kind);
        }
        for path in declared_file_capabilities {
            let Some(path) = normalized_absolute_path(&path) else {
                continue;
            };
            self.declared_file_capabilities.insert(path);
        }
        Ok(())
    }

    /// Require `interpreter` to be available at this point in the transaction.
    ///
    /// Unavailability is the typed [`CcsHookInterpreterUnavailable`]; a
    /// projected resolution failure (for example a symlink loop) is
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

    /// Whether `interpreter` reaches an executable node in the projected
    /// state.
    fn available(&self, interpreter: &str) -> anyhow::Result<bool> {
        let Some(path) = normalized_absolute_path(interpreter) else {
            return Ok(false);
        };
        let resolved = self.resolve_projected(&path)?;
        // A declared `File` capability is a claim, not payload authority: it
        // authorizes only when the projected payload also materializes an
        // executable node at the same path. A declaration alone never does.
        if self.declared_file_capabilities.contains(&path)
            && self.resolved_introduced_executable(&resolved)?
        {
            return Ok(true);
        }
        Ok(match resolved {
            Resolved::Introduced(IntroducedNodeKind::Executable) => true,
            Resolved::Introduced(IntroducedNodeKind::Hardlink { target }) => {
                self.hardlink_target_is_introduced_executable(&target)?
            }
            Resolved::RootFile { executable } => executable,
            Resolved::Missing | Resolved::Introduced(_) => false,
        })
    }

    /// Resolve one normalized absolute package spelling against the projected
    /// state, consulting the pre-transaction root one component at a time.
    ///
    /// A prefix an earlier element removed is unreachable even when the
    /// pre-transaction root still has it. An introduced symlink is spliced
    /// lexically (an absolute target is root-relative, a relative target
    /// resolves against the link's parent) and the walk restarts so every
    /// redirected prefix is re-checked. Symlink-depth overflow is a resolution
    /// error, never unavailability.
    /// Record the parents materialization creates for an introduced path.
    ///
    /// A parent removed earlier in the transaction is recreated as a real
    /// directory; any other missing parent is implied and defers to an
    /// existing root node at resolution time.
    fn record_implied_parents(&mut self, path: &str) {
        let components = path_components(path).into_iter().collect::<Vec<_>>();
        for depth in 1..components.len() {
            let ancestor = absolute_spelling(&components[..depth]);
            if self.introduced.contains_key(&ancestor) {
                continue;
            }
            let kind = if self.removed.remove(&ancestor) {
                IntroducedNodeKind::Directory
            } else {
                IntroducedNodeKind::ImpliedDirectory
            };
            self.introduced.insert(ancestor, kind);
        }
    }

    /// Whether the pre-transaction selected root has any node at `path`,
    /// without following it.
    fn root_has_node(&self, path: &str) -> anyhow::Result<bool> {
        match fs::symlink_metadata(self.root.join(path.trim_start_matches('/'))) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => {
                Err(error).with_context(|| format!("failed to inspect selected-root path {path}"))
            }
        }
    }

    fn resolve_projected(&self, path: &str) -> anyhow::Result<Resolved> {
        let mut pending: VecDeque<String> = path_components(path);
        let mut prefix: Vec<String> = Vec::new();
        let mut symlink_depth = 0usize;

        while let Some(component) = pending.pop_front() {
            prefix.push(component);
            let prefix_path = absolute_spelling(&prefix);
            let is_final = pending.is_empty();

            if self.removed.contains(&prefix_path) && !self.introduced.contains_key(&prefix_path) {
                return Ok(Resolved::Missing);
            }

            if let Some(kind) = self.introduced.get(&prefix_path) {
                if let IntroducedNodeKind::Symlink { target } = kind {
                    symlink_depth += 1;
                    check_symlink_depth(symlink_depth, path)?;
                    pending = splice_symlink_target(&prefix_path, target, pending, path)?;
                    prefix.clear();
                    continue;
                }
                match kind {
                    // An implied parent defers to any existing root node.
                    IntroducedNodeKind::ImpliedDirectory if self.root_has_node(&prefix_path)? => {}
                    IntroducedNodeKind::Directory | IntroducedNodeKind::ImpliedDirectory => {
                        if is_final {
                            return Ok(Resolved::Missing);
                        }
                        continue;
                    }
                    _ if is_final => return Ok(Resolved::Introduced(kind.clone())),
                    _ => return Ok(Resolved::Missing),
                }
            }

            // No projected node and not removed: consult the pre-transaction
            // root for this one component without following it through the
            // host.
            let candidate = self.root.join(prefix_path.trim_start_matches('/'));
            let metadata = match fs::symlink_metadata(&candidate) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(Resolved::Missing);
                }
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("failed to inspect selected-root path {prefix_path}")
                    });
                }
            };
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                symlink_depth += 1;
                check_symlink_depth(symlink_depth, path)?;
                let target = fs::read_link(&candidate).with_context(|| {
                    format!("failed to read selected-root symlink {prefix_path}")
                })?;
                let target = target.to_str().ok_or_else(|| {
                    conary_core::Error::InvalidPath(format!(
                        "selected-root symlink {prefix_path} target is not UTF-8"
                    ))
                })?;
                pending = splice_symlink_target(&prefix_path, target, pending, path)?;
                prefix.clear();
                continue;
            }
            if file_type.is_dir() {
                continue;
            }
            if is_final && file_type.is_file() {
                return Ok(Resolved::RootFile {
                    executable: metadata.permissions().mode() & 0o111 != 0,
                });
            }
            // A non-directory root node cannot have children, and an
            // intermediate missing ancestor was handled above.
            return Ok(Resolved::Missing);
        }

        // The walk consumed a directory leaf: it is not an executable.
        Ok(Resolved::Missing)
    }

    /// Whether the resolver landed on a node the projected payload
    /// materializes and that can execute.
    fn resolved_introduced_executable(&self, resolved: &Resolved) -> anyhow::Result<bool> {
        Ok(match resolved {
            Resolved::Introduced(IntroducedNodeKind::Executable) => true,
            Resolved::Introduced(IntroducedNodeKind::Hardlink { target }) => {
                self.hardlink_target_is_introduced_executable(target)?
            }
            _ => false,
        })
    }

    /// Whether a hardlink's target resolves, through the same projected
    /// resolver, to an introduced executable. A hardlink shares its target's
    /// inode, so it never resolves into the pre-transaction root.
    fn hardlink_target_is_introduced_executable(&self, target: &str) -> anyhow::Result<bool> {
        let Some(target) = normalized_absolute_path(target) else {
            return Ok(false);
        };
        Ok(matches!(
            self.resolve_projected(&target)?,
            Resolved::Introduced(IntroducedNodeKind::Executable)
        ))
    }
}

/// Splice a symlink target into the remaining walk. An absolute target is
/// root-relative; a relative target resolves against the link's parent. The
/// walk restarts from the root so every spliced prefix is re-checked against
/// the projected state.
fn splice_symlink_target(
    link: &str,
    target: &str,
    remaining: VecDeque<String>,
    package_path: &str,
) -> anyhow::Result<VecDeque<String>> {
    let mut resolved: Vec<String> = if target.starts_with('/') {
        Vec::new()
    } else {
        let mut parent = path_components(link);
        parent.pop_back();
        parent.into()
    };
    for component in Path::new(target).components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(part) => resolved.push(part.to_string_lossy().into_owned()),
            Component::ParentDir => {
                if resolved.pop().is_none() {
                    let error = conary_core::Error::PathTraversal(format!(
                        "package path {package_path} escapes the selected root through symlink {link} -> {target}"
                    ));
                    return Err(error.into());
                }
            }
            Component::Prefix(_) => {
                let error = conary_core::Error::PathTraversal(format!(
                    "package path {package_path} has unsupported symlink target {target}"
                ));
                return Err(error.into());
            }
        }
    }
    let mut spliced: VecDeque<String> = resolved.into();
    spliced.extend(remaining);
    Ok(spliced)
}

/// Split one normalized absolute package spelling into its components.
fn path_components(path: &str) -> VecDeque<String> {
    Path::new(path)
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

/// Render one component prefix as its normalized absolute spelling.
fn absolute_spelling(components: &[String]) -> String {
    format!("/{}", components.join("/"))
}

/// Bound projected symlink redirects the same way the selected-root resolver
/// does; overflow is a resolution error, never unavailability.
fn check_symlink_depth(depth: usize, path: &str) -> anyhow::Result<()> {
    if depth > MAX_SELECTED_ROOT_SYMLINK_DEPTH {
        let error = conary_core::Error::PathTraversal(format!(
            "package path {path} exceeds {MAX_SELECTED_ROOT_SYMLINK_DEPTH} projected selected-root symlinks"
        ));
        return Err(error.into());
    }
    Ok(())
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
    PreRemove,
}

impl std::fmt::Display for HookPhase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::PostInstall => "post-install",
            Self::PreRemove => "pre-remove",
        })
    }
}

/// One lifecycle hook interpreter an element must be able to run, tagged with
/// the phase that runs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HookInterpreter {
    pub phase: HookPhase,
    pub interpreter: String,
}

/// Collect the interpreters a manifest's hooks need, in phase order: the
/// post-install hook runs now, and the pre-remove hook runs against the same
/// final projected state once this install completes.
pub(super) fn hook_interpreters(hooks: &Hooks) -> Vec<HookInterpreter> {
    let mut interpreters = Vec::new();
    if let Some(hook) = hooks.post_install.as_ref() {
        interpreters.push(HookInterpreter {
            phase: HookPhase::PostInstall,
            interpreter: hook.interpreter.clone(),
        });
    }
    if let Some(hook) = hooks.pre_remove.as_ref() {
        interpreters.push(HookInterpreter {
            phase: HookPhase::PreRemove,
            interpreter: hook.interpreter.clone(),
        });
    }
    interpreters
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
