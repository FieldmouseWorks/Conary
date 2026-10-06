// crates/conary-core/src/launch/error.rs

//! Typed launcher refusals and failures.
//!
//! Every refusal comes from the filesystem or the kernel; the launcher never
//! opens a database. Exit statuses follow the `env(1)` and container-runtime
//! convention so a launcher failure is distinguishable from the target's own
//! status: 125 for the launcher, 126 when the target cannot be executed, and
//! 127 when it does not exist.

use std::fmt;
use std::path::PathBuf;

use nix::errno::Errno;

use super::policy::{HostBindKind, LauncherMountPoint};

/// Exit status for a launcher refusal or setup failure.
pub const EXIT_LAUNCHER_FAILURE: i32 = 125;
/// Exit status when the target exists but cannot be executed.
pub const EXIT_TARGET_NOT_EXECUTABLE: i32 = 126;
/// Exit status when the target does not exist in the launch tree.
pub const EXIT_TARGET_NOT_FOUND: i32 = 127;

/// Kernel setting that disables user namespaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsernsSetting {
    /// `kernel.unprivileged_userns_clone=0` (Debian-family kernels).
    UnprivilegedUsernsClone,
    /// `user.max_user_namespaces=0`.
    MaxUserNamespaces,
}

impl fmt::Display for UsernsSetting {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::UnprivilegedUsernsClone => "kernel.unprivileged_userns_clone=0",
            Self::MaxUserNamespaces => "user.max_user_namespaces=0",
        })
    }
}

/// The first namespace operation that needs a capability inside the new user
/// namespace. On Ubuntu with `kernel.apparmor_restrict_unprivileged_userns=1`
/// `unshare` succeeds but these fail, so they are the probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeStage {
    /// Writing `setgroups`, `uid_map`, or `gid_map` for the new namespace.
    IdentityMap,
    /// The first mount operation (`rprivate` on `/`).
    MountProbe,
}

impl fmt::Display for ProbeStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::IdentityMap => "identity map write",
            Self::MountProbe => "first mount",
        })
    }
}

/// Mount assembly step that failed after the namespace probe succeeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountStep {
    MakePrivate,
    CloneHostSource(HostBindKind),
    CloneTree,
    AttachTree,
    ResolveTarget,
    Scaffold,
    Tmpfs,
    MirrorEntry,
    AttachHostBind(HostBindKind),
    ReadOnly,
    PivotRoot,
    DetachOldRoot,
}

impl fmt::Display for MountStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MakePrivate => f.write_str("make mounts private"),
            Self::CloneHostSource(kind) => write!(f, "clone host {kind}"),
            Self::CloneTree => f.write_str("clone launch tree"),
            Self::AttachTree => f.write_str("attach launch tree"),
            Self::ResolveTarget => f.write_str("resolve mount target"),
            Self::Scaffold => f.write_str("create mount point"),
            Self::Tmpfs => f.write_str("mount launcher tmpfs"),
            Self::MirrorEntry => f.write_str("mirror directory entry"),
            Self::AttachHostBind(kind) => write!(f, "attach host {kind}"),
            Self::ReadOnly => f.write_str("make mount read-only"),
            Self::PivotRoot => f.write_str("pivot_root"),
            Self::DetachOldRoot => f.write_str("detach old root"),
        }
    }
}

/// Why `conary-exec` refused or failed to run a command.
#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    #[error("no command was given to run")]
    MissingCommand,

    #[error("argument contains a NUL byte: {0:?}")]
    InvalidArgument(std::ffi::OsString),

    #[error("user namespaces are disabled on this host ({setting})")]
    UserNamespacesDisabled { setting: UsernsSetting },

    #[error(
        "the user namespace was created but its capabilities were denied at the {stage} \
         ({errno}); on Ubuntu, AppArmor restricts unprivileged user namespaces and the \
         conary-exec AppArmor profile must be installed and loaded"
    )]
    NamespaceCapabilitiesDenied { stage: ProbeStage, errno: Errno },

    #[error("cannot create user and mount namespaces: {0}")]
    NamespaceUnavailable(Errno),

    #[error("cannot write the namespace {file} map: {errno}")]
    IdentityMapFailed { file: &'static str, errno: Errno },

    #[error("launch tree {} does not exist", path.display())]
    TreeMissing { path: PathBuf },

    #[error("launch tree {} is not a directory", path.display())]
    TreeNotDirectory { path: PathBuf },

    #[error("cannot open launch tree {}: {errno}", path.display())]
    TreeInaccessible { path: PathBuf, errno: Errno },

    #[error("launch tree {} was replaced while the launcher started", path.display())]
    TreeChangedDuringLaunch { path: PathBuf },

    #[error("launch tree {} is on a noexec mount", path.display())]
    TreeOnNoexecMount { path: PathBuf },

    #[error("launch tree {} has no {mount_point} directory for the launcher to mount on", tree.display())]
    TreeMissingMountPoint {
        tree: PathBuf,
        mount_point: LauncherMountPoint,
    },

    #[error("cannot determine the working directory: {0}")]
    WorkingDirectoryUnavailable(Errno),

    #[error(
        "working directory {} is outside the directories shared with the launch tree \
         (home, /tmp, /var/tmp, /run/user/<uid>, /run/host); its read-only host view is {}",
        cwd.display(),
        host_view.display()
    )]
    WorkingDirectoryOutsideBoundSet { cwd: PathBuf, host_view: PathBuf },

    #[error("cannot enter working directory {} inside the launch tree: {errno}", path.display())]
    EnterWorkingDirectory { path: PathBuf, errno: Errno },

    #[error("home directory mount point {} is blocked by a non-directory in the launch tree", path.display())]
    HomeMountPointBlocked { path: PathBuf },

    #[error("mount setup failed ({step}) at {}: {errno}", path.display())]
    MountSetup {
        step: MountStep,
        path: PathBuf,
        errno: Errno,
    },

    #[error("command {command:?} was not found in the launch tree")]
    TargetNotFound { command: std::ffi::OsString },

    #[error("command {command:?} is not executable in the launch tree")]
    TargetNotExecutable { command: std::ffi::OsString },

    #[error("cannot execute {}: {errno}", path.display())]
    ExecFailed { path: PathBuf, errno: Errno },
}

impl LaunchError {
    /// Process exit status that reports this error.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::TargetNotFound { .. } => EXIT_TARGET_NOT_FOUND,
            Self::TargetNotExecutable { .. } | Self::ExecFailed { .. } => {
                EXIT_TARGET_NOT_EXECUTABLE
            }
            _ => EXIT_LAUNCHER_FAILURE,
        }
    }

    /// Stable machine identifier for this refusal class.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::MissingCommand => "missing_command",
            Self::InvalidArgument(_) => "invalid_argument",
            Self::UserNamespacesDisabled { .. } => "user_namespaces_disabled",
            Self::NamespaceCapabilitiesDenied { .. } => "namespace_capabilities_denied",
            Self::NamespaceUnavailable(_) => "namespace_unavailable",
            Self::IdentityMapFailed { .. } => "identity_map_failed",
            Self::TreeMissing { .. } => "tree_missing",
            Self::TreeNotDirectory { .. } => "tree_not_directory",
            Self::TreeInaccessible { .. } => "tree_inaccessible",
            Self::TreeChangedDuringLaunch { .. } => "tree_changed_during_launch",
            Self::TreeOnNoexecMount { .. } => "tree_on_noexec_mount",
            Self::TreeMissingMountPoint { .. } => "tree_missing_mount_point",
            Self::WorkingDirectoryUnavailable(_) => "working_directory_unavailable",
            Self::WorkingDirectoryOutsideBoundSet { .. } => "working_directory_outside_bound_set",
            Self::EnterWorkingDirectory { .. } => "enter_working_directory",
            Self::HomeMountPointBlocked { .. } => "home_mount_point_blocked",
            Self::MountSetup { .. } => "mount_setup",
            Self::TargetNotFound { .. } => "target_not_found",
            Self::TargetNotExecutable { .. } => "target_not_executable",
            Self::ExecFailed { .. } => "exec_failed",
        }
    }
}
