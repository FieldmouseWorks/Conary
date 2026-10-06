// crates/conary-core/src/container/mount_api.rs

//! Thin wrappers over the Linux descriptor-based mount API (`open_tree`,
//! `move_mount`, `mount_setattr`), shared by the scriptlet sandbox and the
//! source-root launcher so both use one audited syscall surface.
//!
//! These run on the sandbox's post-fork child path: no logging, locks, or
//! subprocesses (see `child_fork_safety`). Each wrapper performs exactly one
//! syscall and reports the raw OS error.

use std::ffi::CStr;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};

// Linux UAPI include/uapi/linux/mount.h (v6.16). libc does not expose the
// move_mount flags for musl; the kernel ABI is identical on both libc targets.
pub(crate) const MOVE_MOUNT_F_EMPTY_PATH: libc::c_uint = 0x0000_0004;
pub(crate) const MOVE_MOUNT_T_EMPTY_PATH: libc::c_uint = 0x0000_0040;

fn raw_dirfd(dirfd: Option<BorrowedFd<'_>>) -> RawFd {
    dirfd.map_or(libc::AT_FDCWD, |fd| fd.as_raw_fd())
}

/// `open_tree(2)`: with `OPEN_TREE_CLONE` this returns a detached copy of the
/// mount at `path` (relative to `dirfd`, or the cwd when `None`).
pub(crate) fn open_tree(
    dirfd: Option<BorrowedFd<'_>>,
    path: &CStr,
    flags: libc::c_uint,
) -> io::Result<OwnedFd> {
    // SAFETY: path is NUL-terminated and dirfd is either AT_FDCWD or a live
    // descriptor borrowed for this synchronous call.
    let fd = unsafe { libc::syscall(libc::SYS_open_tree, raw_dirfd(dirfd), path.as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful open_tree returns a fresh descriptor we now own.
    Ok(unsafe { OwnedFd::from_raw_fd(fd as RawFd) })
}

/// `move_mount(2)` from a mount descriptor to a target path or descriptor.
pub(crate) fn move_mount(
    from: BorrowedFd<'_>,
    to_dirfd: Option<BorrowedFd<'_>>,
    to_path: &CStr,
    flags: libc::c_uint,
) -> io::Result<()> {
    // SAFETY: both descriptors are live for this synchronous call and both
    // C strings are NUL-terminated.
    let result = unsafe {
        libc::syscall(
            libc::SYS_move_mount,
            from.as_raw_fd(),
            c"".as_ptr(),
            raw_dirfd(to_dirfd),
            to_path.as_ptr(),
            flags,
        )
    };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// `mount_setattr(2)`: set only the requested attributes. Unlike a legacy
/// remount, it never tries to clear locked `nosuid`/`nodev`/`noexec` flags
/// inherited by a user namespace.
pub(crate) fn mount_setattr(
    dirfd: Option<BorrowedFd<'_>>,
    path: &CStr,
    flags: libc::c_uint,
    attr: &libc::mount_attr,
) -> io::Result<()> {
    // SAFETY: path and the initialized attribute stay valid for the
    // synchronous syscall; dirfd is AT_FDCWD or a live borrowed descriptor.
    let result = unsafe {
        libc::syscall(
            libc::SYS_mount_setattr,
            raw_dirfd(dirfd),
            path.as_ptr(),
            flags,
            attr,
            std::mem::size_of::<libc::mount_attr>(),
        )
    };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// A `mount_attr` that sets `attr_set` and changes nothing else.
pub(crate) fn set_only(attr_set: u64) -> libc::mount_attr {
    libc::mount_attr {
        attr_set,
        attr_clr: 0,
        propagation: 0,
        userns_fd: 0,
    }
}
