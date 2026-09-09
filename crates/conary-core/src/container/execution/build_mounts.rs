// crates/conary-core/src/container/execution/build_mounts.rs

//! Explicit build-directory projections. Detached mounts are opened before
//! fork, mapped by the privileged parent during the user-namespace handshake,
//! and attached only in the child's mount namespace. No host chown is needed.

use super::sandbox_error;
use crate::container::{BindMount, BindMountIdentity};
use crate::error::Result;
use nix::unistd::{Pid, Uid};
use std::ffi::CString;
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

pub(super) struct PreparedBuildMounts(Vec<Option<PreparedBuildMount>>);

struct PreparedBuildMount {
    fd: OwnedFd,
    readonly: bool,
}

impl PreparedBuildMounts {
    pub(super) fn prepare(mounts: &[BindMount]) -> Result<Self> {
        let mut prepared = Vec::with_capacity(mounts.len());
        for mount in mounts {
            if !Uid::effective().is_root()
                || mount.identity == BindMountIdentity::Host
                || (mount.identity == BindMountIdentity::BuildInput && !mount.source.exists())
            {
                prepared.push(None);
                continue;
            }
            let source = CString::new(mount.source.as_os_str().as_bytes())
                .map_err(|error| sandbox_error(format!("Invalid build mount: {error}")))?;
            // SAFETY: source is NUL-terminated; successful open_tree returns
            // an owned, close-on-exec detached mount descriptor.
            let fd = unsafe {
                libc::syscall(
                    libc::SYS_open_tree,
                    libc::AT_FDCWD,
                    source.as_ptr(),
                    libc::OPEN_TREE_CLONE | libc::OPEN_TREE_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(sandbox_error(format!(
                    "Cannot prepare build mount {}: {}",
                    mount.source.display(),
                    std::io::Error::last_os_error()
                )));
            }
            // SAFETY: fd is a fresh descriptor returned above.
            let fd = unsafe { OwnedFd::from_raw_fd(fd as i32) };
            let metadata = File::from(fd.try_clone()?).metadata()?;
            if !metadata.is_dir() {
                return Err(sandbox_error("Build workspace mount must be a directory"));
            }
            prepared.push(Some(PreparedBuildMount {
                fd,
                readonly: !mount.writable,
            }));
        }
        Ok(Self(prepared))
    }

    pub(super) fn map_into(&self, child: Pid) -> Result<()> {
        if self.0.iter().all(Option::is_none) {
            return Ok(());
        }
        let namespace = File::open(format!("/proc/{child}/ns/user"))?;
        for mount in self.0.iter().flatten() {
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

            // SAFETY: the initialized attribute and empty C string are valid
            // for this synchronous syscall. The child waits for the ack before
            // attaching its inherited reference to the same detached mount.
            let result = unsafe {
                libc::syscall(
                    libc::SYS_mount_setattr,
                    mount.fd.as_raw_fd(),
                    c"".as_ptr(),
                    libc::AT_EMPTY_PATH,
                    &attr,
                    std::mem::size_of::<libc::mount_attr>(),
                )
            };
            if result < 0 {
                return Err(sandbox_error(format!(
                    "Cannot map build mount into sandbox identity: {}",
                    std::io::Error::last_os_error()
                )));
            }
        }
        Ok(())
    }

    pub(super) fn contains(&self, index: usize) -> bool {
        self.0[index].is_some()
    }

    /// Post-fork: attach a prepared mount without resolving the source again.
    pub(super) fn attach(&self, index: usize, target: &Path) -> Result<bool> {
        let Some(mount) = &self.0[index] else {
            return Ok(false);
        };
        let target = CString::new(target.as_os_str().as_bytes())
            .map_err(|error| sandbox_error(format!("Invalid build target: {error}")))?;
        // SAFETY: fd is a live detached mount and both C strings are valid.
        let result = unsafe {
            libc::syscall(
                libc::SYS_move_mount,
                mount.fd.as_raw_fd(),
                c"".as_ptr(),
                libc::AT_FDCWD,
                target.as_ptr(),
                libc::MOVE_MOUNT_F_EMPTY_PATH,
            )
        };
        if result < 0 {
            return Err(sandbox_error(format!(
                "Cannot attach mapped build mount: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(true)
    }
}
