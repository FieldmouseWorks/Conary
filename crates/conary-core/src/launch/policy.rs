// crates/conary-core/src/launch/policy.rs

//! Pure launcher policy: kernel-setting evaluation, the typed host-bind plan,
//! the home decision, and working-directory mapping. Nothing here touches
//! namespaces or mounts, so every rule is unit-testable.

use std::ffi::OsStr;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use super::error::{LaunchError, UsernsSetting};

/// Directories every launch tree must provide for the launcher to mount on.
/// The tree is read-only, so the launcher cannot create them itself; S3's
/// launch-tree materializer guarantees them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LauncherMountPoint {
    Proc,
    Dev,
    Sys,
    Tmp,
    VarTmp,
    Etc,
    Run,
}

impl LauncherMountPoint {
    pub const ALL: [Self; 7] = [
        Self::Proc,
        Self::Dev,
        Self::Sys,
        Self::Tmp,
        Self::VarTmp,
        Self::Etc,
        Self::Run,
    ];

    /// Path inside the tree, without a leading slash.
    pub fn relative(self) -> &'static str {
        match self {
            Self::Proc => "proc",
            Self::Dev => "dev",
            Self::Sys => "sys",
            Self::Tmp => "tmp",
            Self::VarTmp => "var/tmp",
            Self::Etc => "etc",
            Self::Run => "run",
        }
    }

    pub fn absolute(self) -> PathBuf {
        Path::new("/").join(self.relative())
    }
}

impl fmt::Display for LauncherMountPoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "/{}", self.relative())
    }
}

/// Host `/etc` files bound over the launcher's tmpfs `/etc`, so identity,
/// name resolution, and time zone come from the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostEtcFile {
    Passwd,
    Group,
    Hosts,
    ResolvConf,
    Localtime,
    MachineId,
}

impl HostEtcFile {
    pub const ALL: [Self; 6] = [
        Self::Passwd,
        Self::Group,
        Self::Hosts,
        Self::ResolvConf,
        Self::Localtime,
        Self::MachineId,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Passwd => "passwd",
            Self::Group => "group",
            Self::Hosts => "hosts",
            Self::ResolvConf => "resolv.conf",
            Self::Localtime => "localtime",
            Self::MachineId => "machine-id",
        }
    }
}

/// One typed host bind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostBindKind {
    Proc,
    Dev,
    Sys,
    Tmp,
    VarTmp,
    Home,
    RuntimeDir,
    HostRoot,
    Etc(HostEtcFile),
}

impl fmt::Display for HostBindKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Proc => f.write_str("/proc"),
            Self::Dev => f.write_str("/dev"),
            Self::Sys => f.write_str("/sys"),
            Self::Tmp => f.write_str("/tmp"),
            Self::VarTmp => f.write_str("/var/tmp"),
            Self::Home => f.write_str("home directory"),
            Self::RuntimeDir => f.write_str("runtime directory"),
            Self::HostRoot => f.write_str("root at /run/host"),
            Self::Etc(file) => write!(f, "/etc/{}", file.name()),
        }
    }
}

/// Whether a missing host source refuses the launch or is skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requirement {
    Required,
    /// Not bound when the host lacks the source (for example no login
    /// session's `/run/user/<uid>`, or a host without `/etc/machine-id`).
    Optional,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostBind {
    pub kind: HostBindKind,
    /// Host path cloned before the tree is attached.
    pub source: PathBuf,
    /// Absolute path inside the launch root.
    pub target: PathBuf,
    pub recursive: bool,
    pub readonly: bool,
    pub requirement: Requirement,
}

/// Snapshot of the kernel settings that gate user namespaces. `None` means
/// the file is absent or unreadable, which never refuses: `unshare` stays the
/// authority for anything these settings do not decide.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsernsSysctls {
    pub unprivileged_userns_clone: Option<u64>,
    pub max_user_namespaces: Option<u64>,
}

impl UsernsSysctls {
    /// Read the settings below a `/proc/sys`-shaped directory.
    pub fn read(proc_sys: &Path) -> Self {
        let read = |relative: &str| {
            std::fs::read_to_string(proc_sys.join(relative))
                .ok()
                .and_then(|contents| parse_sysctl_value(&contents))
        };
        Self {
            unprivileged_userns_clone: read("kernel/unprivileged_userns_clone"),
            max_user_namespaces: read("user/max_user_namespaces"),
        }
    }

    /// The refusal these settings impose on a caller with `euid`.
    /// `unprivileged_userns_clone` does not apply to real root.
    pub fn refusal(&self, euid: u32) -> Option<LaunchError> {
        if self.max_user_namespaces == Some(0) {
            return Some(LaunchError::UserNamespacesDisabled {
                setting: UsernsSetting::MaxUserNamespaces,
            });
        }
        if euid != 0 && self.unprivileged_userns_clone == Some(0) {
            return Some(LaunchError::UserNamespacesDisabled {
                setting: UsernsSetting::UnprivilegedUsernsClone,
            });
        }
        None
    }
}

/// Parse a single-integer sysctl file (`"1\n"`).
pub fn parse_sysctl_value(contents: &str) -> Option<u64> {
    contents.trim().parse().ok()
}

/// The caller's home, as the passwd entry declares it and as it resolves on
/// the host. They differ when the declared path crosses a symlink, such as
/// `/home` -> `/var/home` on image-based hosts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasswdHome {
    pub declared: PathBuf,
    pub resolved: PathBuf,
}

/// Why the caller's home is not bound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HomeSkip {
    /// No passwd entry for the caller (for example sssd or homed users).
    NoPasswdEntry,
    /// The passwd entry names a path that does not exist on the host.
    Missing(PathBuf),
    /// The passwd entry is not an absolute normal path.
    NotAbsolute(PathBuf),
    /// The home is, contains, or lies inside a launcher mount point (`/`,
    /// `/tmp`, `/run`, ...), so binding it would replace launcher state.
    OverlapsLauncherMount { home: PathBuf, mount_point: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HomeBind {
    Bound(PasswdHome),
    Skipped(HomeSkip),
}

/// Launcher-owned mount points a home must not overlap.
fn launcher_owned_mounts() -> Vec<PathBuf> {
    let mut mounts: Vec<PathBuf> = LauncherMountPoint::ALL
        .iter()
        .map(|mount_point| mount_point.absolute())
        .collect();
    mounts.push(PathBuf::from("/"));
    mounts
}

fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

fn is_absolute_normal(path: &Path) -> bool {
    let mut components = path.components();
    components.next() == Some(Component::RootDir)
        && components.all(|component| matches!(component, Component::Normal(_)))
}

/// Decide whether the passwd home is bound. `lookup` is the passwd result:
/// `None` when there is no entry, else the declared path and its host
/// resolution (`None` when it does not exist).
pub fn decide_home(lookup: Option<(PathBuf, Option<PathBuf>)>) -> HomeBind {
    let Some((declared, resolved)) = lookup else {
        return HomeBind::Skipped(HomeSkip::NoPasswdEntry);
    };
    if !is_absolute_normal(&declared) {
        return HomeBind::Skipped(HomeSkip::NotAbsolute(declared));
    }
    let Some(resolved) = resolved else {
        return HomeBind::Skipped(HomeSkip::Missing(declared));
    };
    for mount_point in launcher_owned_mounts() {
        // "/" contains everything; only an exact "/" home overlaps it.
        let collides = if mount_point == Path::new("/") {
            declared == mount_point || resolved == mount_point
        } else {
            overlaps(&declared, &mount_point) || overlaps(&resolved, &mount_point)
        };
        if collides {
            return HomeBind::Skipped(HomeSkip::OverlapsLauncherMount {
                home: declared,
                mount_point,
            });
        }
    }
    HomeBind::Bound(PasswdHome { declared, resolved })
}

/// Absolute path of the caller's runtime directory.
pub fn runtime_dir(uid: u32) -> PathBuf {
    PathBuf::from(format!("/run/user/{uid}"))
}

/// The ordered typed host-bind plan for a caller.
pub fn host_bind_plan(uid: u32, home: &HomeBind) -> Vec<HostBind> {
    let dir = |kind, path: &str, readonly, requirement| HostBind {
        kind,
        source: PathBuf::from(path),
        target: PathBuf::from(path),
        recursive: true,
        readonly,
        requirement,
    };
    let mut plan = vec![
        // A fresh procfs would need a PID namespace and fails under a masked
        // /proc, so the host's is bound recursively with its own flags.
        dir(HostBindKind::Proc, "/proc", false, Requirement::Required),
        dir(HostBindKind::Dev, "/dev", false, Requirement::Required),
        dir(HostBindKind::Sys, "/sys", true, Requirement::Required),
        dir(HostBindKind::Tmp, "/tmp", false, Requirement::Required),
        dir(
            HostBindKind::VarTmp,
            "/var/tmp",
            false,
            Requirement::Required,
        ),
    ];
    if let HomeBind::Bound(home) = home {
        plan.push(HostBind {
            kind: HostBindKind::Home,
            source: home.resolved.clone(),
            target: home.declared.clone(),
            recursive: true,
            readonly: false,
            requirement: Requirement::Required,
        });
    }
    let runtime = runtime_dir(uid);
    plan.push(HostBind {
        kind: HostBindKind::RuntimeDir,
        source: runtime.clone(),
        target: runtime,
        recursive: true,
        readonly: false,
        requirement: Requirement::Optional,
    });
    plan.push(HostBind {
        kind: HostBindKind::HostRoot,
        source: PathBuf::from("/"),
        target: PathBuf::from("/run/host"),
        recursive: true,
        readonly: true,
        requirement: Requirement::Required,
    });
    for file in HostEtcFile::ALL {
        let path = Path::new("/etc").join(file.name());
        plan.push(HostBind {
            kind: HostBindKind::Etc(file),
            source: path.clone(),
            target: path,
            recursive: false,
            readonly: true,
            requirement: Requirement::Optional,
        });
    }
    plan
}

/// A host directory visible inside the launch root, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundDirectory {
    pub host: PathBuf,
    pub inside: PathBuf,
}

/// Directories whose host paths keep their meaning inside the root.
pub fn bound_directories(uid: u32, home: &HomeBind) -> Vec<BoundDirectory> {
    let same = |path: PathBuf| BoundDirectory {
        host: path.clone(),
        inside: path,
    };
    let mut bound = Vec::new();
    if let HomeBind::Bound(home) = home {
        bound.push(BoundDirectory {
            host: home.resolved.clone(),
            inside: home.declared.clone(),
        });
    }
    bound.push(same(PathBuf::from("/tmp")));
    bound.push(same(PathBuf::from("/var/tmp")));
    bound.push(same(runtime_dir(uid)));
    // A nested launch: the outer root's /run/host is the host root, which the
    // inner launch binds at the same place.
    bound.push(same(PathBuf::from("/run/host")));
    bound
}

/// Map the caller's (canonical) working directory into the launch root.
/// A directory outside every bound directory is refused, naming it and its
/// read-only `/run/host` view; it is never silently re-rooted.
pub fn map_working_directory(cwd: &Path, bound: &[BoundDirectory]) -> Result<PathBuf, LaunchError> {
    bound
        .iter()
        .filter_map(|dir| {
            cwd.strip_prefix(&dir.host)
                .ok()
                .map(|rest| (dir.host.as_os_str().len(), dir.inside.join(rest)))
        })
        .max_by_key(|(specificity, _)| *specificity)
        .map(|(_, inside)| inside)
        .ok_or_else(|| LaunchError::WorkingDirectoryOutsideBoundSet {
            cwd: cwd.to_path_buf(),
            host_view: Path::new("/run/host").join(cwd.strip_prefix("/").unwrap_or(cwd)),
        })
}

/// Search order for `command`, following `execvp`: a name containing `/` is
/// used as given; otherwise each `PATH` entry is tried, an empty entry
/// meaning the working directory. Without `PATH`, glibc's `/bin:/usr/bin`.
pub fn exec_candidates(command: &OsStr, path_var: Option<&OsStr>) -> Vec<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    if command.as_bytes().contains(&b'/') {
        return vec![PathBuf::from(command)];
    }
    let path_var = path_var.unwrap_or(OsStr::new("/bin:/usr/bin"));
    path_var
        .as_bytes()
        .split(|byte| *byte == b':')
        .map(|entry| {
            let dir = if entry.is_empty() {
                Path::new(".")
            } else {
                Path::new(OsStr::from_bytes(entry))
            };
            dir.join(command)
        })
        .collect()
}

#[cfg(test)]
#[path = "policy/tests.rs"]
mod tests;
