// crates/conary-core/src/launch/prepare.rs

//! Checks that run before any namespace exists: the launch tree, the
//! caller's identity and home, and the working-directory mapping. Every
//! refusal here leaves the process untouched.

use std::os::fd::{AsFd, OwnedFd};
use std::path::{Path, PathBuf};

use nix::errno::Errno;
use nix::fcntl::{OFlag, open};
use nix::sys::stat::{Mode, fstat};
use nix::sys::statvfs::{FsFlags, fstatvfs};
use nix::unistd::{Uid, User, getegid, geteuid};

use super::error::LaunchError;
use super::mounts::resolve_in_root;
use super::policy::{
    HomeBind, HostBind, LauncherMountPoint, bound_directories, decide_home, host_bind_plan,
    map_working_directory,
};

/// Everything the namespace phase needs, gathered without side effects.
pub(super) struct PreparedLaunch {
    pub(super) tree: PathBuf,
    pub(super) tree_fd: OwnedFd,
    pub(super) uid: u32,
    pub(super) gid: u32,
    pub(super) home: HomeBind,
    pub(super) plan: Vec<HostBind>,
    pub(super) cwd_inside: PathBuf,
}

/// Open the launch tree as an `O_PATH` directory. The descriptor, not the
/// path, is what gets bound, so the tree is resolved exactly once.
pub fn open_launch_tree(tree: &Path) -> Result<OwnedFd, LaunchError> {
    open(
        tree,
        OFlag::O_PATH | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC,
        Mode::empty(),
    )
    .map_err(|errno| match errno {
        Errno::ENOENT => LaunchError::TreeMissing {
            path: tree.to_path_buf(),
        },
        Errno::ENOTDIR => LaunchError::TreeNotDirectory {
            path: tree.to_path_buf(),
        },
        errno => LaunchError::TreeInaccessible {
            path: tree.to_path_buf(),
            errno,
        },
    })
}

/// Refuse a tree on a `noexec` mount: nothing in it could run.
pub fn refuse_noexec(tree: &Path, flags: FsFlags) -> Result<(), LaunchError> {
    if flags.contains(FsFlags::ST_NOEXEC) {
        return Err(LaunchError::TreeOnNoexecMount {
            path: tree.to_path_buf(),
        });
    }
    Ok(())
}

/// Validate an opened launch tree: executable mount, and every launcher
/// mount point present as a directory when resolved inside the tree.
pub fn check_launch_tree(tree: &Path, tree_fd: &OwnedFd) -> Result<(), LaunchError> {
    let stats = fstatvfs(tree_fd).map_err(|errno| LaunchError::TreeInaccessible {
        path: tree.to_path_buf(),
        errno,
    })?;
    refuse_noexec(tree, stats.flags())?;
    for mount_point in LauncherMountPoint::ALL {
        resolve_in_root(
            tree_fd.as_fd(),
            Path::new(mount_point.relative()),
            libc::O_DIRECTORY,
        )
        .map_err(|_| LaunchError::TreeMissingMountPoint {
            tree: tree.to_path_buf(),
            mount_point,
        })?;
    }
    Ok(())
}

/// The caller's passwd home, decided against the launcher's policy.
fn caller_home(uid: u32) -> HomeBind {
    let lookup = match User::from_uid(Uid::from_raw(uid)) {
        Ok(Some(user)) => {
            let resolved = std::fs::canonicalize(&user.dir).ok();
            Some((user.dir, resolved))
        }
        Ok(None) | Err(_) => None,
    };
    decide_home(lookup)
}

/// Re-open the tree after `unshare(CLONE_NEWNS)`: a descriptor opened in the
/// parent mount namespace cannot be cloned or mounted on from the new one.
/// The re-opened tree must be the inode the checks above approved.
pub(super) fn reopen_in_namespace(prepared: &PreparedLaunch) -> Result<OwnedFd, LaunchError> {
    let reopened = open_launch_tree(&prepared.tree)?;
    let identity = |fd: &OwnedFd| {
        fstat(fd)
            .map(|stat| (stat.st_dev, stat.st_ino))
            .map_err(|errno| LaunchError::TreeInaccessible {
                path: prepared.tree.clone(),
                errno,
            })
    };
    if identity(&reopened)? != identity(&prepared.tree_fd)? {
        return Err(LaunchError::TreeChangedDuringLaunch {
            path: prepared.tree.clone(),
        });
    }
    Ok(reopened)
}

pub(super) fn prepare(tree: &Path) -> Result<PreparedLaunch, LaunchError> {
    let tree_fd = open_launch_tree(tree)?;
    check_launch_tree(tree, &tree_fd)?;

    let uid = geteuid().as_raw();
    let gid = getegid().as_raw();
    let home = caller_home(uid);
    let cwd = nix::unistd::getcwd().map_err(LaunchError::WorkingDirectoryUnavailable)?;
    let cwd_inside = map_working_directory(&cwd, &bound_directories(uid, &home))?;
    let plan = host_bind_plan(uid, &home);
    Ok(PreparedLaunch {
        tree: tree.to_path_buf(),
        tree_fd,
        uid,
        gid,
        home,
        plan,
        cwd_inside,
    })
}

#[cfg(test)]
#[path = "prepare/tests.rs"]
mod tests;
