// apps/conary/src/commands/install/ccs_hook_interpreter.rs
//! Transaction-ordered availability of CCS hook interpreters.

use anyhow::Context;
use conary_core::filesystem::selected_root::selected_root_executable;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Transaction-ordered availability of CCS hook interpreters in one selected root.
pub(super) struct HookInterpreterLedger {
    root: PathBuf,
    /// Normalized absolute paths and `File` capabilities introduced by
    /// transaction elements already applied in execution order.
    introduced: BTreeSet<String>,
    /// Normalized absolute paths whose final provider an earlier element removed.
    removed: BTreeSet<String>,
}

impl HookInterpreterLedger {
    pub(super) fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            introduced: BTreeSet::new(),
            removed: BTreeSet::new(),
        }
    }

    /// Record one element's payload boundary: paths it removes (old-only
    /// paths of an upgrade/removal) then paths and File capabilities it installs.
    pub(super) fn apply_element(
        &mut self,
        removed_paths: impl IntoIterator<Item = String>,
        introduced: impl IntoIterator<Item = String>,
    ) {
        for path in removed_paths {
            let Some(path) = normalized_absolute_path(&path) else {
                continue;
            };
            self.introduced.remove(&path);
            self.removed.insert(path);
        }
        for path in introduced {
            let Some(path) = normalized_absolute_path(&path) else {
                continue;
            };
            self.removed.remove(&path);
            self.introduced.insert(path);
        }
    }

    /// Require `interpreter` to be available at this point in the transaction.
    ///
    /// Unavailability is the typed [`CcsHookInterpreterUnavailable`]; a
    /// selected-root resolution failure (for example a symlink loop) is
    /// propagated rather than reported as a missing interpreter.
    pub(super) fn require(
        &self,
        package: &str,
        version: &str,
        phase: HookPhase,
        interpreter: &str,
    ) -> anyhow::Result<()> {
        let available = match normalized_absolute_path(interpreter) {
            Some(normalized) if self.introduced.contains(&normalized) => true,
            Some(normalized) if self.removed.contains(&normalized) => false,
            Some(normalized) => selected_root_executable(&self.root, &normalized)
                .with_context(|| {
                    format!(
                        "failed to resolve {phase} interpreter {interpreter} for {package} {version} in the selected root"
                    )
                })?
                .is_some(),
            None => false,
        };
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
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    const PRESENT: &str = "/usr/bin/hook-interpreter";

    /// Unwrap the typed refusal; any other error fails the test.
    fn typed(error: anyhow::Error) -> CcsHookInterpreterUnavailable {
        error
            .downcast()
            .expect("refusal must be the typed CcsHookInterpreterUnavailable")
    }

    fn ledger_with_executable() -> (tempfile::TempDir, HookInterpreterLedger) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(PRESENT.trim_start_matches('/'));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        let ledger = HookInterpreterLedger::new(root.path());
        (root, ledger)
    }

    #[test]
    fn present_interpreter_in_root_is_available() {
        let (_root, ledger) = ledger_with_executable();

        ledger
            .require("pkg", "1.0.0", HookPhase::PostInstall, PRESENT)
            .expect("an executable in the selected root is available");
    }

    #[test]
    fn absent_interpreter_is_rejected_with_the_exact_reason() {
        let (_root, ledger) = ledger_with_executable();
        let missing = "/usr/bin/absent";

        // Positive control: the identical fixture finds the present path.
        ledger
            .require("pkg", "1.0.0", HookPhase::PostInstall, PRESENT)
            .expect("the fixture's present executable is available");

        let error = ledger
            .require("pkg", "1.0.0", HookPhase::PostInstall, missing)
            .map_err(typed)
            .expect_err("an absent interpreter must be refused");
        assert_eq!(error.package, "pkg");
        assert_eq!(error.version, "1.0.0");
        assert_eq!(error.phase, HookPhase::PostInstall);
        assert_eq!(error.interpreter, missing);
    }

    #[test]
    fn own_element_can_introduce_an_absent_interpreter() {
        let root = tempfile::tempdir().unwrap();
        let mut ledger = HookInterpreterLedger::new(root.path());
        let planned = "/opt/planned/sh";

        // Negative control on the same fixture: without the element it is absent.
        assert!(
            ledger
                .require("pkg", "1.0.0", HookPhase::PostInstall, planned)
                .map_err(typed)
                .is_err()
        );

        ledger.apply_element(Vec::new(), vec![planned.to_string()]);
        ledger
            .require("pkg", "1.0.0", HookPhase::PostInstall, planned)
            .expect("a path introduced by this element is planned availability");
    }

    #[test]
    fn earlier_element_path_capability_authorizes_a_later_interpreter() {
        let root = tempfile::tempdir().unwrap();
        let mut ledger = HookInterpreterLedger::new(root.path());

        // The typed provider arrives as this element's File capability.
        ledger.apply_element(Vec::new(), vec!["/usr/bin/sh".to_string()]);
        ledger
            .require(
                "provider-dependent",
                "2.0.0",
                HookPhase::PostInstall,
                "/usr/bin/sh",
            )
            .expect("an earlier element's File capability authorizes the interpreter");
    }

    #[test]
    fn removal_by_an_earlier_element_beats_root_presence() {
        let (_root, mut ledger) = ledger_with_executable();

        // Positive control: before removal the same fixture is available.
        ledger
            .require("pkg", "1.0.0", HookPhase::PostInstall, PRESENT)
            .expect("the fixture's executable is available before removal");

        ledger.apply_element(vec![PRESENT.to_string()], Vec::new());
        let error = ledger
            .require("pkg", "1.0.0", HookPhase::PostInstall, PRESENT)
            .map_err(typed)
            .expect_err("a removed final provider must not authorize a later interpreter");
        assert_eq!(error.interpreter, PRESENT);
        assert_eq!(error.phase, HookPhase::PostInstall);
    }

    #[test]
    fn reintroduction_after_removal_restores_availability() {
        let root = tempfile::tempdir().unwrap();
        let mut ledger = HookInterpreterLedger::new(root.path());

        ledger.apply_element(vec![PRESENT.to_string()], Vec::new());
        assert!(
            ledger
                .require("pkg", "1.0.0", HookPhase::PostInstall, PRESENT)
                .map_err(typed)
                .is_err()
        );

        ledger.apply_element(Vec::new(), vec![PRESENT.to_string()]);
        ledger
            .require("pkg", "1.0.0", HookPhase::PostInstall, PRESENT)
            .expect("a later element reintroducing the path restores availability");
    }
}
