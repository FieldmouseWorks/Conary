// crates/conary-core/src/container/execution/build_mounts.rs

//! Explicit build-directory projections. Detached mounts are opened before
//! fork, mapped by the privileged parent during the user-namespace handshake,
//! and attached only in the child's mount namespace. No host chown is needed.

use super::sandbox_error;
use crate::container::mount_api::{self, MOVE_MOUNT_F_EMPTY_PATH};
use crate::container::{BindMount, BindMountIdentity};
use crate::error::Result;
use nix::unistd::{Pid, Uid};
use std::ffi::CString;
use std::fs::File;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

pub(super) struct PreparedBuildMounts(Vec<PreparedMount>);

/// Per-bind state recorded before fork. An optional input absent at preparation
/// stays absent without a fresh source lookup, so it cannot later be attached
/// as an ordinary host bind.
enum PreparedMount {
    /// Detached, identity-mapped clone ready to attach by descriptor.
    Detached(DetachedBuildMount),
    /// Ordinary host bind, or a caller that keeps its own host identity.
    Direct,
    /// Optional build input that did not exist when preparation inspected it.
    AbsentInput,
}

struct DetachedBuildMount {
    fd: OwnedFd,
    readonly: bool,
}

/// What mount setup must do for one configured bind.
pub(super) enum BuildMountPlan {
    /// Optional input absent at preparation: skip without touching the source.
    SkipAbsentInput,
    /// Direct bind whose source is missing now: skip.
    SkipMissingSource,
    /// Attach the detached, identity-mapped clone by descriptor.
    AttachDetached,
    /// Plain bind of the source path with the caller's identity.
    BindSource,
}

impl PreparedBuildMounts {
    pub(super) fn prepare(mounts: &[BindMount]) -> Result<Self> {
        let mut prepared = Vec::with_capacity(mounts.len());
        for mount in mounts {
            if mount.identity == BindMountIdentity::Host {
                prepared.push(PreparedMount::Direct);
                continue;
            }
            let exists = mount.source.try_exists().map_err(|error| {
                sandbox_error(format!(
                    "Cannot inspect build mount {}: {error}",
                    mount.source.display()
                ))
            })?;
            if !exists {
                if mount.identity == BindMountIdentity::BuildInput {
                    // Recorded absence: mount setup skips this entry on this
                    // state even if the path appears before the child runs.
                    prepared.push(PreparedMount::AbsentInput);
                    continue;
                }
                return Err(sandbox_error(format!(
                    "Required build workspace is missing: {}",
                    mount.source.display()
                )));
            }
            // An ordinary caller keeps its host UID in the user-namespace map;
            // its own files already have the right identity through plain binds.
            if !Uid::effective().is_root() {
                prepared.push(PreparedMount::Direct);
                continue;
            }
            let source = CString::new(mount.source.as_os_str().as_bytes())
                .map_err(|error| sandbox_error(format!("Invalid build mount: {error}")))?;
            let fd = mount_api::open_tree(
                None,
                &source,
                libc::OPEN_TREE_CLONE | libc::OPEN_TREE_CLOEXEC,
            )
            .map_err(|error| {
                sandbox_error(format!(
                    "Cannot prepare build mount {}: {error}",
                    mount.source.display()
                ))
            })?;
            let metadata = File::from(fd.try_clone()?).metadata()?;
            if !metadata.is_dir() {
                return Err(sandbox_error("Build workspace mount must be a directory"));
            }
            prepared.push(PreparedMount::Detached(DetachedBuildMount {
                fd,
                readonly: !mount.writable,
            }));
        }
        Ok(Self(prepared))
    }

    pub(super) fn map_into(&self, child: Pid) -> Result<()> {
        if self.0.iter().all(|mount| mount.detached().is_none()) {
            return Ok(());
        }
        let namespace = File::open(format!("/proc/{child}/ns/user"))?;
        for mount in self.0.iter().filter_map(PreparedMount::detached) {
            let attr = libc::mount_attr {
                attr_set: libc::MOUNT_ATTR_IDMAP
                    | if mount.readonly {
                        libc::MOUNT_ATTR_RDONLY
                    } else {
                        0
                    },
                attr_clr: 0,
                // Detached clones retain the source peer group. Make each private
                // before attachment so nested mounts cannot propagate back to
                // the caller through a shared workspace mount.
                propagation: libc::MS_PRIVATE,
                userns_fd: namespace.as_raw_fd() as u64,
            };

            // The child waits for the ack before attaching its inherited
            // reference to the same detached mount.
            mount_api::mount_setattr(
                Some(mount.fd.as_fd()),
                c"",
                libc::AT_EMPTY_PATH as libc::c_uint,
                &attr,
            )
            .map_err(|error| {
                sandbox_error(format!(
                    "Cannot map build mount into sandbox identity: {error}"
                ))
            })?;
        }
        Ok(())
    }

    /// Decide mount setup for one configured bind without re-resolving sources
    /// that preparation already classified. `source_exists` runs only for
    /// direct binds; an absent optional input is skipped on its recorded state.
    pub(super) fn plan(
        &self,
        index: usize,
        source_exists: impl FnOnce() -> bool,
    ) -> BuildMountPlan {
        match &self.0[index] {
            PreparedMount::AbsentInput => BuildMountPlan::SkipAbsentInput,
            PreparedMount::Detached(_) => BuildMountPlan::AttachDetached,
            PreparedMount::Direct => {
                if source_exists() {
                    BuildMountPlan::BindSource
                } else {
                    BuildMountPlan::SkipMissingSource
                }
            }
        }
    }

    /// Post-fork: attach a prepared mount without resolving the source again.
    /// Only [`BuildMountPlan::AttachDetached`] entries carry one.
    pub(super) fn attach(&self, index: usize, target: &Path) -> Result<()> {
        let PreparedMount::Detached(mount) = &self.0[index] else {
            return Err(sandbox_error(
                "Build mount attachment requires a detached prepared mount",
            ));
        };
        let target = CString::new(target.as_os_str().as_bytes())
            .map_err(|error| sandbox_error(format!("Invalid build target: {error}")))?;
        mount_api::move_mount(mount.fd.as_fd(), None, &target, MOVE_MOUNT_F_EMPTY_PATH)
            .map_err(|error| sandbox_error(format!("Cannot attach mapped build mount: {error}")))?;
        Ok(())
    }
}

impl PreparedMount {
    fn detached(&self) -> Option<&DetachedBuildMount> {
        match self {
            Self::Detached(mount) => Some(mount),
            Self::Direct | Self::AbsentInput => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_build_input_is_not_classified_as_absent() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("loop");
        std::os::unix::fs::symlink("loop", &source).unwrap();
        let result = PreparedBuildMounts::prepare(&[BindMount::build_input(&source, "/input")]);
        let Err(error) = result else {
            panic!("symlink loop must fail preparation")
        };
        assert!(
            error.to_string().contains("Cannot inspect build mount"),
            "{error}"
        );
    }

    #[test]
    fn missing_optional_input_is_skipped() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("missing");
        let mounts =
            PreparedBuildMounts::prepare(&[BindMount::build_input(&source, "/input")]).unwrap();
        assert!(matches!(
            mounts.plan(0, || false),
            BuildMountPlan::SkipAbsentInput
        ));
    }

    #[test]
    fn absent_optional_input_stays_skipped_after_source_appears() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("input");
        let mounts =
            PreparedBuildMounts::prepare(&[BindMount::build_input(&source, "/input")]).unwrap();

        // The source appears after preparation but before mount setup.
        std::fs::create_dir(&source).unwrap();

        let mut probed = false;
        let plan = mounts.plan(0, || {
            probed = true;
            true
        });
        assert!(matches!(plan, BuildMountPlan::SkipAbsentInput));
        assert!(
            !probed,
            "absent optional input must not be re-resolved during mount setup"
        );
    }

    #[test]
    fn host_bind_source_is_planned_as_a_direct_bind() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("host");
        std::fs::create_dir(&source).unwrap();
        let mounts =
            PreparedBuildMounts::prepare(&[BindMount::readonly(&source, "/host")]).unwrap();
        assert!(matches!(
            mounts.plan(0, || true),
            BuildMountPlan::BindSource
        ));
    }

    #[test]
    fn host_bind_with_missing_source_is_skipped() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("missing");
        let mounts =
            PreparedBuildMounts::prepare(&[BindMount::readonly(&source, "/host")]).unwrap();
        let mut probed = false;
        let plan = mounts.plan(0, || {
            probed = true;
            false
        });
        assert!(matches!(plan, BuildMountPlan::SkipMissingSource));
        assert!(probed, "direct binds still re-check the source");
    }

    #[test]
    fn missing_workspace_is_refused_before_identity_selection() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("missing");
        let result = PreparedBuildMounts::prepare(&[BindMount::build_workspace(&source, "/build")]);
        let Err(error) = result else {
            panic!("required workspace must exist")
        };
        assert!(
            error
                .to_string()
                .contains("Required build workspace is missing"),
            "{error}"
        );
    }
}
