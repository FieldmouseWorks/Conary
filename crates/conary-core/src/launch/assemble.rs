// crates/conary-core/src/launch/assemble.rs

//! Build the launch root inside the private mount namespace, then pivot into
//! it. Order matters and is part of the contract:
//!
//! 1. Clone every host source first, so no clone can contain launcher mounts.
//! 2. Clone the tree through its `O_PATH` descriptor, make it read-only with
//!    `mount_setattr(AT_RECURSIVE)` (locked flags survive), and attach it onto
//!    itself.
//! 3. Create the home mount point, mirroring a read-only tree directory onto
//!    a tmpfs when the tree has no place for it.
//! 4. Mount tmpfs on `/run` and `/etc`; `/etc` mirrors the selected source
//!    and gains bind targets for the host files.
//! 5. Attach the host binds, then seal the launcher tmpfs mounts read-only.
//! 6. `pivot_root(".", ".")` and detach the old root.

use std::ffi::OsStr;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::{Component, Path, PathBuf};

use nix::errno::Errno;
use nix::mount::{MntFlags, umount2};

use super::EtcSource;
use super::error::{LaunchError, MountStep};
use super::mounts::{
    attach, c_path, clone_mount, make_directory, make_file, new_tmpfs, open_child, populate_mirror,
    resolve_in_root, set_readonly, snapshot_directory,
};
use super::policy::{HomeBind, HostBind, HostBindKind, Requirement};
use super::prepare::{PreparedLaunch, reopen_in_namespace};

fn errno(error: &io::Error) -> Errno {
    Errno::from_raw(error.raw_os_error().unwrap_or(libc::EIO))
}

fn failed(step: MountStep, path: impl Into<PathBuf>) -> impl FnOnce(io::Error) -> LaunchError {
    let path = path.into();
    move |error| LaunchError::MountSetup {
        step,
        path,
        errno: errno(&error),
    }
}

fn relative(path: &Path) -> &Path {
    path.strip_prefix("/").unwrap_or(path)
}

/// Clone every planned host source. Optional sources the host lacks are not
/// bound; a missing required source refuses the launch.
fn clone_host_sources(plan: &[HostBind]) -> Result<Vec<(HostBind, OwnedFd)>, LaunchError> {
    let mut clones = Vec::with_capacity(plan.len());
    for bind in plan {
        let source = c_path(&bind.source)
            .map_err(failed(MountStep::CloneHostSource(bind.kind), &bind.source))?;
        let mount = match clone_mount(None, &source, bind.recursive, true) {
            Ok(mount) => mount,
            Err(error)
                if bind.requirement == Requirement::Optional && errno(&error) == Errno::ENOENT =>
            {
                continue;
            }
            Err(error) => {
                return Err(failed(MountStep::CloneHostSource(bind.kind), &bind.source)(
                    error,
                ));
            }
        };
        if bind.readonly {
            set_readonly(mount.as_fd(), bind.recursive)
                .map_err(failed(MountStep::ReadOnly, &bind.source))?;
        }
        clones.push((bind.clone(), mount));
    }
    Ok(clones)
}

/// Mirror the read-only directory `parent` onto a fresh tmpfs so new mount
/// points can be created in it. Returns the tmpfs, already attached.
fn scaffold(parent: BorrowedFd<'_>, at: &Path) -> Result<OwnedFd, LaunchError> {
    let entries = snapshot_directory(parent, &[]).map_err(failed(MountStep::MirrorEntry, at))?;
    let tmpfs = new_tmpfs().map_err(failed(MountStep::Tmpfs, at))?;
    attach(tmpfs.as_fd(), parent).map_err(failed(MountStep::Scaffold, at))?;
    populate_mirror(tmpfs.as_fd(), entries)
        .map_err(|(name, error)| failed(MountStep::MirrorEntry, at.join(name))(error))?;
    Ok(tmpfs)
}

/// Make `path` (absolute, normal components) a directory inside the root.
/// Returns a replacement root descriptor when the root itself was mirrored.
fn ensure_directory(
    root: BorrowedFd<'_>,
    path: &Path,
    sealed_later: &mut Vec<OwnedFd>,
) -> Result<Option<OwnedFd>, LaunchError> {
    let mut new_root: Option<OwnedFd> = None;
    let mut existing = PathBuf::new();
    for component in relative(path).components() {
        let Component::Normal(name) = component else {
            return Err(LaunchError::HomeMountPointBlocked {
                path: path.to_path_buf(),
            });
        };
        let current_root = new_root.as_ref().map_or(root, |fd| fd.as_fd());
        let candidate = existing.join(name);
        let absolute = Path::new("/").join(&candidate);
        match resolve_in_root(current_root, &candidate, libc::O_DIRECTORY) {
            Ok(_) => {
                existing = candidate;
                continue;
            }
            Err(error) if errno(&error) == Errno::ENOTDIR => {
                return Err(LaunchError::HomeMountPointBlocked { path: absolute });
            }
            Err(error) if errno(&error) != Errno::ENOENT => {
                return Err(failed(MountStep::ResolveTarget, absolute)(error));
            }
            Err(_) => {}
        }
        let parent_path = Path::new("/").join(&existing);
        let parent = resolve_in_root(current_root, &existing, libc::O_DIRECTORY)
            .map_err(failed(MountStep::ResolveTarget, &parent_path))?;
        match make_directory(parent.as_fd(), name, 0o755) {
            Ok(()) => {}
            // EROFS is checked before permissions: the parent is tree content.
            Err(error) if errno(&error) == Errno::EROFS => {
                let tmpfs = scaffold(parent.as_fd(), &parent_path)?;
                make_directory(tmpfs.as_fd(), name, 0o755)
                    .map_err(failed(MountStep::Scaffold, &absolute))?;
                if existing.as_os_str().is_empty() {
                    new_root = Some(
                        tmpfs
                            .try_clone()
                            .map_err(failed(MountStep::Scaffold, "/"))?,
                    );
                }
                sealed_later.push(tmpfs);
            }
            Err(error) => return Err(failed(MountStep::Scaffold, absolute)(error)),
        }
        existing = candidate;
    }
    Ok(new_root)
}

/// Mount a tmpfs on the directory `name` inside the root.
fn mount_tmpfs(root: BorrowedFd<'_>, at: &str) -> Result<(OwnedFd, OwnedFd), LaunchError> {
    let target = resolve_in_root(root, Path::new(at), libc::O_DIRECTORY)
        .map_err(failed(MountStep::ResolveTarget, Path::new("/").join(at)))?;
    let tmpfs = new_tmpfs().map_err(failed(MountStep::Tmpfs, Path::new("/").join(at)))?;
    Ok((tmpfs, target))
}

fn mount_run(root: BorrowedFd<'_>, uid: u32) -> Result<OwnedFd, LaunchError> {
    let (run, target) = mount_tmpfs(root, "run")?;
    attach(run.as_fd(), target.as_fd()).map_err(failed(MountStep::Tmpfs, "/run"))?;
    let scaffold =
        |result: io::Result<()>, at: &str| result.map_err(failed(MountStep::Scaffold, at));
    scaffold(
        make_directory(run.as_fd(), OsStr::new("host"), 0o755),
        "/run/host",
    )?;
    scaffold(
        make_directory(run.as_fd(), OsStr::new("user"), 0o755),
        "/run/user",
    )?;
    let user = open_child(run.as_fd(), OsStr::new("user"))
        .map_err(failed(MountStep::Scaffold, "/run/user"))?;
    scaffold(
        make_directory(user.as_fd(), OsStr::new(&uid.to_string()), 0o700),
        "/run/user/<uid>",
    )?;
    Ok(run)
}

fn mount_etc(
    root: BorrowedFd<'_>,
    source: &EtcSource,
    host_files: &[&OsStr],
) -> Result<OwnedFd, LaunchError> {
    let etc_source = match source {
        EtcSource::Tree => resolve_in_root(root, Path::new("etc"), libc::O_DIRECTORY)
            .map_err(failed(MountStep::ResolveTarget, "/etc"))?,
        EtcSource::Directory(path) => nix::fcntl::open(
            path,
            nix::fcntl::OFlag::O_PATH
                | nix::fcntl::OFlag::O_DIRECTORY
                | nix::fcntl::OFlag::O_CLOEXEC,
            nix::sys::stat::Mode::empty(),
        )
        .map_err(|errno| LaunchError::MountSetup {
            step: MountStep::MirrorEntry,
            path: path.clone(),
            errno,
        })?,
    };
    let entries = snapshot_directory(etc_source.as_fd(), host_files)
        .map_err(failed(MountStep::MirrorEntry, "/etc"))?;
    let (etc, target) = mount_tmpfs(root, "etc")?;
    attach(etc.as_fd(), target.as_fd()).map_err(failed(MountStep::Tmpfs, "/etc"))?;
    populate_mirror(etc.as_fd(), entries).map_err(|(name, error)| {
        failed(MountStep::MirrorEntry, Path::new("/etc").join(name))(error)
    })?;
    for name in host_files {
        make_file(etc.as_fd(), name)
            .map_err(failed(MountStep::Scaffold, Path::new("/etc").join(name)))?;
    }
    Ok(etc)
}

/// Assemble the launch root. Returns the descriptor of its top mount.
pub(super) fn assemble_root(
    prepared: &PreparedLaunch,
    etc: &EtcSource,
) -> Result<OwnedFd, LaunchError> {
    let host = clone_host_sources(&prepared.plan)?;

    let tree_fd = reopen_in_namespace(prepared)?;
    let tree = clone_mount(Some(tree_fd.as_fd()), c"", true, false)
        .map_err(failed(MountStep::CloneTree, &prepared.tree))?;
    set_readonly(tree.as_fd(), true).map_err(failed(MountStep::ReadOnly, &prepared.tree))?;
    attach(tree.as_fd(), tree_fd.as_fd()).map_err(failed(MountStep::AttachTree, &prepared.tree))?;

    let mut root = tree;
    let mut sealed_later = Vec::new();
    if let HomeBind::Bound(home) = &prepared.home
        && let Some(new_root) = ensure_directory(root.as_fd(), &home.declared, &mut sealed_later)?
    {
        root = new_root;
    }

    sealed_later.push(mount_run(root.as_fd(), prepared.uid)?);
    let host_etc: Vec<&OsStr> = host
        .iter()
        .filter_map(|(bind, _)| match bind.kind {
            HostBindKind::Etc(file) => Some(OsStr::new(file.name())),
            _ => None,
        })
        .collect();
    sealed_later.push(mount_etc(root.as_fd(), etc, &host_etc)?);

    for (bind, mount) in &host {
        let extra = if bind.recursive { libc::O_DIRECTORY } else { 0 };
        let target = resolve_in_root(root.as_fd(), relative(&bind.target), extra)
            .map_err(failed(MountStep::ResolveTarget, &bind.target))?;
        attach(mount.as_fd(), target.as_fd())
            .map_err(failed(MountStep::AttachHostBind(bind.kind), &bind.target))?;
    }
    for tmpfs in &sealed_later {
        set_readonly(tmpfs.as_fd(), false)
            .map_err(failed(MountStep::ReadOnly, "launcher tmpfs"))?;
    }
    Ok(root)
}

/// Make `root` the process root and detach the host root.
pub(super) fn pivot_into(root: &OwnedFd) -> Result<(), LaunchError> {
    let pivot = |error: Errno| LaunchError::MountSetup {
        step: MountStep::PivotRoot,
        path: "/".into(),
        errno: error,
    };
    nix::unistd::fchdir(root).map_err(pivot)?;
    // SAFETY: both arguments are the NUL-terminated literal ".".
    if unsafe { libc::syscall(libc::SYS_pivot_root, c".".as_ptr(), c".".as_ptr()) } < 0 {
        return Err(pivot(Errno::last()));
    }
    // The old root is now stacked on top of the new one at ".".
    umount2(".", MntFlags::MNT_DETACH).map_err(|errno| LaunchError::MountSetup {
        step: MountStep::DetachOldRoot,
        path: "/".into(),
        errno,
    })?;
    nix::unistd::chdir("/").map_err(pivot)
}
