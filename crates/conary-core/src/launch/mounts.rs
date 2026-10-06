// crates/conary-core/src/launch/mounts.rs

//! Descriptor-based mount primitives for the launcher. Every target is
//! resolved with `openat2(RESOLVE_IN_ROOT)` against the launch root, so an
//! absolute symlink inside the tree can never steer a mount onto a host path
//! before `pivot_root`.

use std::ffi::{CStr, CString, OsStr, OsString};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;

use crate::container::mount_api::{self, MOVE_MOUNT_F_EMPTY_PATH, MOVE_MOUNT_T_EMPTY_PATH};

// Linux UAPI include/uapi/linux/mount.h (v6.16); libc does not export these.
const FSOPEN_CLOEXEC: libc::c_uint = 0x0000_0001;
const FSCONFIG_SET_STRING: libc::c_uint = 1;
const FSCONFIG_CMD_CREATE: libc::c_uint = 6;
const FSMOUNT_CLOEXEC: libc::c_uint = 0x0000_0001;

/// `struct open_how` from include/uapi/linux/openat2.h. libc marks its copy
/// non-exhaustive, so it cannot be built with a struct expression.
#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

pub(super) fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))
}

fn c_name(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes()).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))
}

fn owned(fd: libc::c_long) -> io::Result<OwnedFd> {
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the caller passes a descriptor a syscall just returned.
    Ok(unsafe { OwnedFd::from_raw_fd(fd as RawFd) })
}

fn check(result: libc::c_int) -> io::Result<()> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Detached copy of the mount at `path` (relative to `dirfd`; an empty path
/// means `dirfd` itself). Symlinks in the final component are followed only
/// for host sources, never for tree entries.
pub(super) fn clone_mount(
    dirfd: Option<BorrowedFd<'_>>,
    path: &CStr,
    recursive: bool,
    follow_final_symlink: bool,
) -> io::Result<OwnedFd> {
    let mut flags = libc::OPEN_TREE_CLONE | libc::OPEN_TREE_CLOEXEC;
    if recursive {
        flags |= libc::AT_RECURSIVE as libc::c_uint;
    }
    if path.is_empty() {
        flags |= libc::AT_EMPTY_PATH as libc::c_uint;
    }
    if !follow_final_symlink {
        flags |= libc::AT_SYMLINK_NOFOLLOW as libc::c_uint;
    }
    mount_api::open_tree(dirfd, path, flags)
}

/// Make the mount behind `fd` (and, if `recursive`, everything below it)
/// read-only without clearing any locked flag.
pub(super) fn set_readonly(fd: BorrowedFd<'_>, recursive: bool) -> io::Result<()> {
    let mut flags = libc::AT_EMPTY_PATH as libc::c_uint;
    if recursive {
        flags |= libc::AT_RECURSIVE as libc::c_uint;
    }
    mount_api::mount_setattr(
        Some(fd),
        c"",
        flags,
        &mount_api::set_only(libc::MOUNT_ATTR_RDONLY),
    )
}

/// Attach a detached mount on top of the inode `target` refers to.
pub(super) fn attach(mount: BorrowedFd<'_>, target: BorrowedFd<'_>) -> io::Result<()> {
    mount_api::move_mount(
        mount,
        Some(target),
        c"",
        MOVE_MOUNT_F_EMPTY_PATH | MOVE_MOUNT_T_EMPTY_PATH,
    )
}

/// A new, detached, `nosuid,nodev` tmpfs whose root has mode 0755.
pub(super) fn new_tmpfs() -> io::Result<OwnedFd> {
    // SAFETY: the literal is NUL-terminated; fsopen returns a new descriptor.
    let context =
        owned(unsafe { libc::syscall(libc::SYS_fsopen, c"tmpfs".as_ptr(), FSOPEN_CLOEXEC) })?;
    // SAFETY: the context descriptor is live and both strings are literals.
    check(unsafe {
        libc::syscall(
            libc::SYS_fsconfig,
            context.as_raw_fd(),
            FSCONFIG_SET_STRING,
            c"mode".as_ptr(),
            c"0755".as_ptr(),
            0,
        )
    } as libc::c_int)?;
    // SAFETY: CMD_CREATE takes no key or value.
    check(unsafe {
        libc::syscall(
            libc::SYS_fsconfig,
            context.as_raw_fd(),
            FSCONFIG_CMD_CREATE,
            std::ptr::null::<libc::c_char>(),
            std::ptr::null::<libc::c_void>(),
            0,
        )
    } as libc::c_int)?;
    // SAFETY: the configured context is live; fsmount returns a new descriptor.
    owned(unsafe {
        libc::syscall(
            libc::SYS_fsmount,
            context.as_raw_fd(),
            FSMOUNT_CLOEXEC,
            (libc::MOUNT_ATTR_NOSUID | libc::MOUNT_ATTR_NODEV) as libc::c_uint,
        )
    })
}

/// Open `relative` inside `root` as if `root` were `/`. `extra` adds open
/// flags (`O_DIRECTORY`); the result is always an `O_PATH` descriptor.
pub(super) fn resolve_in_root(
    root: BorrowedFd<'_>,
    relative: &Path,
    extra: libc::c_int,
) -> io::Result<OwnedFd> {
    let path = if relative.as_os_str().is_empty() {
        CString::from(c".")
    } else {
        c_path(relative)?
    };
    let how = OpenHow {
        flags: (libc::O_PATH | libc::O_CLOEXEC | extra) as u64,
        mode: 0,
        resolve: libc::RESOLVE_IN_ROOT | libc::RESOLVE_NO_MAGICLINKS,
    };
    // SAFETY: root is live, path is NUL-terminated, and `how` is initialized
    // with its exact size.
    owned(unsafe {
        libc::syscall(
            libc::SYS_openat2,
            root.as_raw_fd(),
            path.as_ptr(),
            &how,
            std::mem::size_of::<OpenHow>(),
        )
    })
}

/// Create directory `name` below `parent`.
pub(super) fn make_directory(parent: BorrowedFd<'_>, name: &OsStr, mode: u32) -> io::Result<()> {
    let name = c_name(name)?;
    // SAFETY: parent is live and name is NUL-terminated.
    check(unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), mode as libc::mode_t) })
}

/// Create an empty regular file `name` below `parent` as a bind target.
pub(super) fn make_file(parent: BorrowedFd<'_>, name: &OsStr) -> io::Result<()> {
    let name = c_name(name)?;
    // SAFETY: parent is live and name is NUL-terminated; the descriptor is
    // closed immediately by OwnedFd.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_CREAT | libc::O_EXCL | libc::O_WRONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o644 as libc::c_uint,
        )
    };
    owned(fd as libc::c_long).map(drop)
}

/// Open `name` directly below `parent` without following a final symlink.
pub(super) fn open_child(parent: BorrowedFd<'_>, name: &OsStr) -> io::Result<OwnedFd> {
    let name = c_name(name)?;
    // SAFETY: parent is live and name is NUL-terminated.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    owned(fd as libc::c_long)
}

fn read_link(parent: BorrowedFd<'_>, name: &OsStr) -> io::Result<OsString> {
    let name = c_name(name)?;
    let mut buffer = vec![0_u8; libc::PATH_MAX as usize];
    // SAFETY: the buffer is writable for its full length.
    let length = unsafe {
        libc::readlinkat(
            parent.as_raw_fd(),
            name.as_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
        )
    };
    if length < 0 {
        return Err(io::Error::last_os_error());
    }
    buffer.truncate(length as usize);
    Ok(OsString::from_vec(buffer))
}

fn make_symlink(parent: BorrowedFd<'_>, name: &OsStr, target: &OsStr) -> io::Result<()> {
    let name = c_name(name)?;
    let target = c_name(target)?;
    // SAFETY: both strings are NUL-terminated and parent is live.
    check(unsafe { libc::symlinkat(target.as_ptr(), parent.as_raw_fd(), name.as_ptr()) })
}

/// One entry of a directory being mirrored onto a launcher tmpfs.
pub(super) enum MirrorEntry {
    Symlink { name: OsString, target: OsString },
    Directory { name: OsString, mount: OwnedFd },
    Other { name: OsString, mount: OwnedFd },
}

impl MirrorEntry {
    fn name(&self) -> &OsStr {
        match self {
            Self::Symlink { name, .. }
            | Self::Directory { name, .. }
            | Self::Other { name, .. } => name,
        }
    }
}

/// Snapshot `directory` (an `O_PATH` directory descriptor) before it is
/// covered: symlinks are copied as text and every other entry becomes a
/// read-only detached clone, so the mirror exposes the original inodes with
/// their own metadata. Entries named in `skip` are left out.
pub(super) fn snapshot_directory(
    directory: BorrowedFd<'_>,
    skip: &[&OsStr],
) -> io::Result<Vec<MirrorEntry>> {
    let listing = std::fs::read_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()))?;
    let mut entries = Vec::new();
    for entry in listing {
        let entry = entry?;
        let name = entry.file_name();
        if skip.contains(&name.as_os_str()) {
            continue;
        }
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            let target = read_link(directory, &name)?;
            entries.push(MirrorEntry::Symlink { name, target });
            continue;
        }
        let mount = clone_mount(Some(directory), &c_name(&name)?, true, false)?;
        set_readonly(mount.as_fd(), true)?;
        entries.push(if file_type.is_dir() {
            MirrorEntry::Directory { name, mount }
        } else {
            MirrorEntry::Other { name, mount }
        });
    }
    Ok(entries)
}

/// Recreate a snapshot inside `destination`, a launcher tmpfs. Returns the
/// name of the entry that failed, if any.
pub(super) fn populate_mirror(
    destination: BorrowedFd<'_>,
    entries: Vec<MirrorEntry>,
) -> Result<(), (OsString, io::Error)> {
    for entry in entries {
        let name = entry.name().to_os_string();
        let result = match &entry {
            MirrorEntry::Symlink { name, target } => make_symlink(destination, name, target),
            MirrorEntry::Directory { name, mount } => make_directory(destination, name, 0o755)
                .and_then(|()| open_child(destination, name))
                .and_then(|target| attach(mount.as_fd(), target.as_fd())),
            MirrorEntry::Other { name, mount } => make_file(destination, name)
                .and_then(|()| open_child(destination, name))
                .and_then(|target| attach(mount.as_fd(), target.as_fd())),
        };
        result.map_err(|error| (name, error))?;
    }
    Ok(())
}
