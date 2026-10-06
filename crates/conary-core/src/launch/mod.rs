// crates/conary-core/src/launch/mod.rs

//! Unprivileged launcher for source-root launch trees (`conary-exec`).
//!
//! Given an already materialized, read-only launch tree, the launcher runs a
//! command from it as the calling user, with no supervisor and no database:
//!
//! 1. `unshare(CLONE_NEWUSER | CLONE_NEWNS)` with no PID namespace; deny
//!    `setgroups` and map the caller's uid and gid to themselves.
//! 2. Make every mount private (this is the capability probe), clone the host
//!    sources, then bind the tree onto itself through its `O_PATH`
//!    descriptor and make it read-only with `mount_setattr(AT_RECURSIVE)`.
//! 3. tmpfs on `/run` and `/etc` (`/etc` mirrors the tree's own `/etc`, or a
//!    composed directory); typed host binds: `/proc`, `/dev`, `/sys`
//!    read-only, `/tmp`, `/var/tmp`, the passwd home, `/run/user/<uid>`, six
//!    host `/etc` files read-only, and host `/` read-only at `/run/host`.
//! 4. `pivot_root`, detach the old root, enter the mapped working directory,
//!    and `execve` the target.
//!
//! The pure rules live in [`policy`]; the namespace and mount mechanics share
//! the descriptor mount API and identity-map writer with the scriptlet
//! sandbox in [`crate::container`].

mod assemble;
pub mod error;
mod exec;
mod mounts;
mod namespace;
pub mod policy;
mod prepare;

use std::convert::Infallible;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub use error::{
    EXIT_LAUNCHER_FAILURE, EXIT_TARGET_NOT_EXECUTABLE, EXIT_TARGET_NOT_FOUND, LaunchError,
    MountStep, ProbeStage, UsernsSetting,
};
pub use exec::search_exhausted;
pub use prepare::{check_launch_tree, open_launch_tree, refuse_noexec};

/// Where the launcher's tmpfs `/etc` takes its entries from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EtcSource {
    /// The launch tree's own `/etc`.
    Tree,
    /// A pre-composed directory (generation `/etc` plus `etc-state`), which
    /// the launch-tree materializer will produce.
    Directory(PathBuf),
}

/// One launch: a read-only tree, its `/etc` source, and the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchRequest {
    pub tree: PathBuf,
    pub etc: EtcSource,
    pub command: Vec<OsString>,
}

fn refuse_by_kernel_settings(euid: u32) -> Result<(), LaunchError> {
    match policy::UsernsSysctls::read(Path::new("/proc/sys")).refusal(euid) {
        Some(refusal) => Err(refusal),
        None => Ok(()),
    }
}

/// Run `request` in this process. Returns only when the launch is refused or
/// the target cannot be executed; on success the process becomes the target.
///
/// The process must be single-threaded: `unshare(CLONE_NEWUSER)` refuses a
/// multithreaded caller.
pub fn launch(request: &LaunchRequest) -> Result<Infallible, LaunchError> {
    if request.command.is_empty() {
        return Err(LaunchError::MissingCommand);
    }
    let prepared = prepare::prepare(&request.tree)?;
    refuse_by_kernel_settings(prepared.uid)?;
    namespace::enter(prepared.uid, prepared.gid)?;
    let root = assemble::assemble_root(&prepared, &request.etc)?;
    assemble::pivot_into(&root)?;
    drop(root);
    nix::unistd::chdir(&prepared.cwd_inside).map_err(|errno| {
        LaunchError::EnterWorkingDirectory {
            path: prepared.cwd_inside.clone(),
            errno,
        }
    })?;
    Err(exec::exec_target(&request.command, &prepared.cwd_inside))
}

/// Probe, in this process, whether the launcher's namespaces are usable:
/// kernel settings, `unshare`, the identity map, and the first mount.
///
/// On success the process is left inside a new user and mount namespace, so
/// call this only from a process that exits right after (`conary-exec
/// --check`).
pub fn probe_in_this_process() -> Result<(), LaunchError> {
    let uid = nix::unistd::geteuid().as_raw();
    let gid = nix::unistd::getegid().as_raw();
    refuse_by_kernel_settings(uid)?;
    namespace::enter(uid, gid)
}

#[cfg(test)]
#[path = "namespace_tests.rs"]
mod namespace_tests;
