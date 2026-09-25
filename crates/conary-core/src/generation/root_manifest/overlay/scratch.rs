// crates/conary-core/src/generation/root_manifest/overlay/scratch.rs

//! Functional selection of where a selected-root OverlayFS session keeps its
//! upper and work directories.
//!
//! Only the upper and work directories must live on a filesystem that can host
//! an OverlayFS upper. The kernel refuses an OverlayFS `upperdir` located on an
//! OverlayFS mount, which every container runtime root is. Selection therefore
//! runs the existing functional profile probe on each candidate in a fixed
//! preference order and admits the first candidate whose complete probe passes.
//! No errno text, mount name, or container detection participates.

use super::{
    SelectedRootOverlayCapabilities, SelectedRootOverlayProfile,
    probe_selected_root_overlay_profile,
};
use nix::mount::{MntFlags, MsFlags, mount, umount2};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Where a selected-root OverlayFS session keeps its upper and work directories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OverlayScratchPlacement {
    /// Upper/work live directly in the session directory on the runtime-root filesystem.
    SessionDirectory,
    /// Upper/work live on a private tmpfs mounted at `<session>/scratch`.
    PrivateTmpfs,
}

impl OverlayScratchPlacement {
    /// Candidates in preference order. Disk-backed scratch is preferred; a
    /// RAM-backed tmpfs is used only when the runtime-root filesystem cannot
    /// host an OverlayFS upper.
    pub const PREFERENCE: [Self; 2] = [Self::SessionDirectory, Self::PrivateTmpfs];
}

/// One tmpfs mounted for the lifetime of a selected-root session.
pub struct MountedScratchTmpfs {
    target: PathBuf,
    mounted: bool,
}

impl MountedScratchTmpfs {
    /// Mount a private tmpfs at `target`, creating `target` first.
    ///
    /// `MS_NODEV` is deliberately omitted: OverlayFS whiteouts are 0/0
    /// character devices created in the upper.
    pub fn mount(target: &Path) -> crate::Result<Self> {
        fs::create_dir_all(target).map_err(|error| {
            crate::Error::IoError(format!(
                "failed to create private tmpfs scratch directory {}: {error}",
                target.display()
            ))
        })?;
        mount(
            Some("tmpfs"),
            target,
            Some("tmpfs"),
            MsFlags::MS_NOSUID,
            Some("mode=0700"),
        )
        .map_err(|error| {
            crate::Error::IoError(format!(
                "failed to mount private tmpfs scratch at {}: {error}",
                target.display()
            ))
        })?;
        Ok(Self {
            target: target.to_path_buf(),
            mounted: true,
        })
    }

    pub fn path(&self) -> &Path {
        &self.target
    }

    /// Strictly unmount the tmpfs and mark it detached.
    pub fn unmount(mut self) -> crate::Result<()> {
        if !self.mounted {
            return Ok(());
        }
        umount2(&self.target, MntFlags::empty()).map_err(|error| {
            crate::Error::IoError(format!(
                "failed to strictly unmount private tmpfs scratch {}: {error}",
                self.target.display()
            ))
        })?;
        self.mounted = false;
        Ok(())
    }
}

impl Drop for MountedScratchTmpfs {
    fn drop(&mut self) {
        if self.mounted {
            let _ = umount2(&self.target, MntFlags::MNT_DETACH);
        }
    }
}

/// Minimal mount-handle contract so selection can be exercised without real
/// mount privileges in tests.
trait ScratchMount: Sized {
    fn path(&self) -> &Path;
    fn unmount(self) -> crate::Result<()>;
}

impl ScratchMount for MountedScratchTmpfs {
    fn path(&self) -> &Path {
        MountedScratchTmpfs::path(self)
    }

    fn unmount(self) -> crate::Result<()> {
        MountedScratchTmpfs::unmount(self)
    }
}

/// A proven scratch location for one selected-root session.
pub struct SelectedRootOverlayScratch {
    capabilities: SelectedRootOverlayCapabilities,
    directory: PathBuf,
    tmpfs: Option<MountedScratchTmpfs>,
}

impl SelectedRootOverlayScratch {
    pub fn capabilities(&self) -> &SelectedRootOverlayCapabilities {
        &self.capabilities
    }

    pub fn placement(&self) -> OverlayScratchPlacement {
        self.capabilities.scratch_placement
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Hand the tmpfs mount (if any) to the session that will own its lifetime.
    pub fn into_parts(
        self,
    ) -> (
        SelectedRootOverlayCapabilities,
        PathBuf,
        Option<MountedScratchTmpfs>,
    ) {
        (self.capabilities, self.directory, self.tmpfs)
    }
}

/// One candidate that failed the functional probe, in the order tried.
#[derive(Debug)]
pub struct OverlayScratchCandidateFailure {
    pub placement: OverlayScratchPlacement,
    pub error: String,
}

/// The placement-independent result of one selection run.
struct ScratchSelection<M> {
    capabilities: SelectedRootOverlayCapabilities,
    directory: PathBuf,
    tmpfs: Option<M>,
}

/// Probe candidates in [`OverlayScratchPlacement::PREFERENCE`] order.
pub fn select_selected_root_overlay_scratch(
    session_dir: &Path,
    profile: &SelectedRootOverlayProfile,
) -> crate::Result<SelectedRootOverlayScratch> {
    let selection = select_with(
        session_dir,
        profile,
        probe_selected_root_overlay_profile,
        MountedScratchTmpfs::mount,
    )?;
    Ok(SelectedRootOverlayScratch {
        capabilities: selection.capabilities,
        directory: selection.directory,
        tmpfs: selection.tmpfs,
    })
}

/// Run the functional probe on every candidate in preference order.
///
/// The probe is never told which candidate it is running on; the placement is
/// stamped onto the returned capabilities by the caller.
fn select_with<M: ScratchMount>(
    session_dir: &Path,
    profile: &SelectedRootOverlayProfile,
    mut probe: impl FnMut(
        &Path,
        &SelectedRootOverlayProfile,
    ) -> crate::Result<SelectedRootOverlayCapabilities>,
    mut mount_tmpfs: impl FnMut(&Path) -> crate::Result<M>,
) -> crate::Result<ScratchSelection<M>> {
    let mut failures = Vec::new();
    for placement in OverlayScratchPlacement::PREFERENCE {
        match placement {
            OverlayScratchPlacement::SessionDirectory => match probe(session_dir, profile) {
                Ok(mut capabilities) => {
                    capabilities.scratch_placement = OverlayScratchPlacement::SessionDirectory;
                    return Ok(ScratchSelection {
                        capabilities,
                        directory: session_dir.to_path_buf(),
                        tmpfs: None,
                    });
                }
                Err(error) => failures.push(OverlayScratchCandidateFailure {
                    placement,
                    error: error.to_string(),
                }),
            },
            OverlayScratchPlacement::PrivateTmpfs => {
                let target = session_dir.join("scratch");
                let mounted = match mount_tmpfs(&target) {
                    Ok(mounted) => mounted,
                    Err(error) => {
                        failures.push(OverlayScratchCandidateFailure {
                            placement,
                            error: error.to_string(),
                        });
                        continue;
                    }
                };
                match probe(mounted.path(), profile) {
                    Ok(mut capabilities) => {
                        capabilities.scratch_placement = OverlayScratchPlacement::PrivateTmpfs;
                        return Ok(ScratchSelection {
                            capabilities,
                            directory: mounted.path().to_path_buf(),
                            tmpfs: Some(mounted),
                        });
                    }
                    Err(error) => {
                        failures.push(OverlayScratchCandidateFailure {
                            placement,
                            error: error.to_string(),
                        });
                        let _ = mounted.unmount();
                    }
                }
            }
        }
    }
    Err(crate::Error::SelectedRootOverlayUnsupported(failures))
}

#[cfg(test)]
mod tests;
