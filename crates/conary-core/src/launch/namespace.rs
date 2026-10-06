// crates/conary-core/src/launch/namespace.rs

//! Enter the launcher's user and mount namespaces and probe that their
//! capabilities are usable.
//!
//! There is deliberately no PID namespace: the target is `execve`d in this
//! process, so stdio, the controlling terminal, signals, and the exit status
//! pass through with no supervisor.

use nix::errno::Errno;
use nix::mount::{MsFlags, mount};
use nix::sched::{CloneFlags, unshare};

use super::error::{LaunchError, ProbeStage};
use crate::container::namespaces::{id_map_line, write_identity_maps};

/// Classify a failure of a probe-stage operation. `EPERM` or `EACCES` after a
/// successful `unshare` means the namespace exists but its capabilities are
/// denied, which is how Ubuntu's AppArmor userns restriction presents.
pub(super) fn classify_probe_failure(
    stage: ProbeStage,
    errno: Errno,
    identity_file: &'static str,
) -> LaunchError {
    match errno {
        Errno::EPERM | Errno::EACCES => LaunchError::NamespaceCapabilitiesDenied { stage, errno },
        _ => match stage {
            ProbeStage::IdentityMap => LaunchError::IdentityMapFailed {
                file: identity_file,
                errno,
            },
            ProbeStage::MountProbe => LaunchError::MountSetup {
                step: super::error::MountStep::MakePrivate,
                path: "/".into(),
                errno,
            },
        },
    }
}

fn errno_of(error: &std::io::Error) -> Errno {
    Errno::from_raw(error.raw_os_error().unwrap_or(libc::EIO))
}

/// `unshare(CLONE_NEWUSER | CLONE_NEWNS)`, map the caller's ids to
/// themselves with `setgroups` denied, and make every mount private. The
/// `rprivate` remount is the capability probe. Must run single-threaded.
///
/// A caller with euid 0 takes the same path with a 0 -> 0 map: host-root
/// files keep their owner, while the target's capabilities are scoped to the
/// new user namespace and the copied host mounts stay locked.
pub(super) fn enter(uid: u32, gid: u32) -> Result<(), LaunchError> {
    unshare(CloneFlags::CLONE_NEWUSER | CloneFlags::CLONE_NEWNS)
        .map_err(LaunchError::NamespaceUnavailable)?;
    write_identity_maps("/proc/self", &id_map_line(uid, uid), &id_map_line(gid, gid)).map_err(
        |(file, error)| {
            classify_probe_failure(ProbeStage::IdentityMap, errno_of(&error), file.name())
        },
    )?;
    mount::<str, str, str, str>(None, "/", None, MsFlags::MS_REC | MsFlags::MS_PRIVATE, None)
        .map_err(|errno| classify_probe_failure(ProbeStage::MountProbe, errno, ""))
}
