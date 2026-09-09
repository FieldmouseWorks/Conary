// crates/conary-core/src/container/execution/root_setup.rs

//! Selected-root, namespace, mount, and enforcement setup for sandboxed children.
//!
//! Everything below [`Sandbox::child_setup_and_execute`] runs after `fork()` and
//! before `exec()`, in a process that inherited the parent's lock state. No
//! `tracing` macro and no subprocess spawn may appear on that path: see
//! [`crate::container::child_safety`] for why, and `child_fork_safety` in the
//! container tests for the check that keeps it true.

use super::credentials::seal_namespace_credentials;
use super::*;
use crate::container::child_safety::{bring_loopback_up, child_diag, format_int, int_buffer};
use crate::container::namespaces::{
    clear_privileged_supplementary_groups, enter_mapped_namespace_root,
};
use std::os::unix::ffi::OsStrExt;

impl Sandbox {
    pub(super) fn run_forked_child(&self, child: ForkedChild<'_>) -> ! {
        let ForkedChild {
            stdin_fd,
            stdout_read_fd,
            stdout_write_fd,
            stderr_read_fd,
            stderr_write_fd,
            userns_request_read_fd,
            userns_ack_write_fd,
            execution,
        } = child;
        drop(stdout_read_fd);
        drop(stderr_read_fd);
        drop(userns_request_read_fd);
        drop(userns_ack_write_fd);

        if unsafe { libc::dup2(stdin_fd, 0) } < 0 {
            child_diag(&[b"failed to attach sandbox stdin"]);
            unsafe { libc::_exit(127) };
        }
        if unsafe { libc::dup2(stdout_write_fd.as_raw_fd(), 1) } < 0 {
            child_diag(&[b"failed to attach sandbox stdout"]);
            unsafe { libc::_exit(127) };
        }
        if unsafe { libc::dup2(stderr_write_fd.as_raw_fd(), 2) } < 0 {
            child_diag(&[b"failed to attach sandbox stderr"]);
            unsafe { libc::_exit(127) };
        }
        drop(stdout_write_fd);
        drop(stderr_write_fd);

        match self.child_setup_and_execute(execution) {
            Ok(code) => unsafe { libc::_exit(code) },
            Err(error) => {
                let message = error.to_string();
                child_diag(&[message.as_bytes()]);
                unsafe { libc::_exit(127) };
            }
        }
    }

    /// Set up the container filesystem with bind mounts.
    pub(super) fn setup_container_fs(&self, root: &Path) -> Result<()> {
        for dir in &[
            "dev", "etc", "proc", "sys", "tmp", "usr", "lib", "lib64", "bin", "sbin", "var",
        ] {
            let path = root.join(dir);
            fs::create_dir_all(&path)?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
        }

        let mut tmp_perms = fs::metadata(root.join("tmp"))?.permissions();
        tmp_perms.set_mode(0o1777);
        fs::set_permissions(root.join("tmp"), tmp_perms)?;

        let dev = root.join("dev");
        for node in &["null", "zero", "urandom", "random"] {
            let path = dev.join(node);
            if !path.exists() {
                File::create(&path)?;
            }
            let mut perms = fs::metadata(&path)?.permissions();
            perms.set_mode(0o666);
            fs::set_permissions(&path, perms)?;
        }

        for bind in &self.config.bind_mounts {
            if !bind.source.exists() {
                continue;
            }
            let relative = bind.target.strip_prefix("/").unwrap_or(&bind.target);
            if relative
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
            {
                return Err(sandbox_error(
                    "Sandbox bind targets must contain only normal path components",
                ));
            }
            let target = root.join(relative);
            let directory = if bind.source.is_dir() {
                target.as_path()
            } else {
                target.parent().unwrap_or(root)
            };
            create_mount_parents(root, directory)?;
            if !bind.source.is_dir() && !target.exists() {
                File::create(&target)?;
            }
        }

        Ok(())
    }

    /// Configure the isolated child and replace it with the requested process.
    pub(super) fn child_setup_and_execute(&self, execution: ChildExecution<'_>) -> Result<i32> {
        let ChildExecution {
            root,
            build_mounts,
            program,
            interpreter_args,
            script_path,
            args,
            env,
            userns_sync,
            deadline,
            enforcement,
        } = execution;
        let script_in_container = script_path.map(|path| {
            path.strip_prefix(root)
                .map(|relative| Path::new("/").join(relative))
                .unwrap_or_else(|_| path.to_path_buf())
        });
        let namespace_flags: &[(bool, CloneFlags)] = &[
            (self.config.isolate_pid, CloneFlags::CLONE_NEWPID),
            (self.config.isolate_uts, CloneFlags::CLONE_NEWUTS),
            (self.config.isolate_ipc, CloneFlags::CLONE_NEWIPC),
            (self.config.isolate_mount, CloneFlags::CLONE_NEWNS),
            (self.config.isolate_network, CloneFlags::CLONE_NEWNET),
        ];
        let flags = namespace_flags
            .iter()
            .filter(|(enabled, _)| *enabled)
            .fold(CloneFlags::empty(), |acc, (_, flag)| acc | *flag);
        let flags_with_user = sandbox_namespace_flags(flags);
        if !flags.contains(CloneFlags::CLONE_NEWNS) {
            return Err(sandbox_error(
                "Isolated execution requires a mount namespace",
            ));
        }
        clear_privileged_supplementary_groups()?;
        unshare(flags_with_user).map_err(|error| {
            sandbox_error(format!(
                "Unshare with mandatory user identity failed: {error}"
            ))
        })?;
        signal_parent_user_namespace_ready(userns_sync.as_ref())?;

        if self.config.isolate_pid {
            match fork_process()
                .map_err(|error| sandbox_error(format!("PID namespace fork failed: {error}")))?
            {
                ForkResult::Parent { child } => {
                    return wait_for_pid_namespace_init(child, deadline);
                }
                ForkResult::Child => {
                    let parent = unsafe { libc::getppid() };
                    set_parent_death_signal(libc::SIGKILL).map_err(|error| {
                        sandbox_error(format!(
                            "failed to bind PID namespace init lifetime to its monitor: {error}"
                        ))
                    })?;
                    if unsafe { libc::getppid() } != parent {
                        return Err(sandbox_error(
                            "PID namespace monitor exited during init setup",
                        ));
                    }
                }
            }
        }

        if self.config.isolate_network
            && let Err(errno) = bring_loopback_up()
        {
            let mut digits = int_buffer();
            child_diag(&[
                b"failed to bring up loopback interface: errno ",
                format_int(&mut digits, errno as i64),
            ]);
        }

        if self.config.isolate_uts && !self.config.hostname.is_empty() {
            let hostname = CString::new(self.config.hostname.as_str()).map_err(|e| {
                execution_error(
                    ScriptletFailureKind::ContractViolation,
                    format!("Invalid hostname: {e}"),
                )
            })?;
            if let Err(err) = sethostname_syscall(&hostname, self.config.hostname.len()) {
                let mut digits = int_buffer();
                child_diag(&[
                    b"sethostname failed: errno ",
                    format_int(&mut digits, err.raw_os_error().unwrap_or(-1) as i64),
                ]);
            }
        }

        self.setup_mount_namespace(root, build_mounts)?;

        // Assemble mounts while the setup process can access its private root.
        // Enter the mapped identity before enforcing and executing the payload.
        enter_mapped_namespace_root()?;
        seal_namespace_credentials()?;

        self.apply_resource_limits()?;

        if let Some(prepared) = enforcement {
            let mode = prepared.mode();
            match enforcement::apply_prepared_enforcement(prepared) {
                Ok(report) => {
                    for warning in &report.warnings {
                        child_diag(&[
                            b"enforcement setup: [",
                            warning.category.as_bytes(),
                            b"] ",
                            warning.message.as_bytes(),
                        ]);
                    }
                }
                Err(error) => {
                    if mode == EnforcementMode::Enforce {
                        return Err(execution_error(
                            ScriptletFailureKind::EnforcementSetupFailed,
                            format!("Capability enforcement failed: {error}"),
                        ));
                    }
                    child_diag(&[b"capability enforcement skipped"]);
                }
            }
        }

        std::env::set_current_dir(&self.config.workdir)
            .map_err(|e| sandbox_error(format!("chdir failed: {e}")))?;

        let mut command = Command::new(program);
        command.args(interpreter_args);
        if let Some(script_in_container) = script_in_container.as_ref() {
            command.arg(script_in_container);
        }
        command
            .args(args)
            .stdin(Stdio::null())
            .env_clear()
            .env("HOME", "/root")
            .env("TERM", "dumb")
            .env("LANG", "C.UTF-8")
            .env("SHELL", "/bin/sh");

        if !env.iter().any(|(key, _)| *key == "PATH") {
            command.env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin");
        }
        for (key, value) in env {
            command.env(*key, *value);
        }

        let error = command.exec();
        Err(execution_error(
            ScriptletFailureKind::ProgramUnavailable,
            format!("Exec failed: {error}"),
        ))
    }

    fn setup_mount_namespace(&self, root: &Path, build_mounts: &PreparedBuildMounts) -> Result<()> {
        mount::<str, str, str, str>(None, "/", None, MsFlags::MS_PRIVATE | MsFlags::MS_REC, None)
            .map_err(|e| sandbox_error(format!("mount --make-rprivate failed: {e}")))?;

        for (index, bind_mount) in self.config.bind_mounts.iter().enumerate() {
            if !build_mounts.contains(index) && !bind_mount.source.exists() {
                // The hot path: a missing optional bind source is ordinary, and
                // this ran on essentially every sandbox start. It was the most
                // frequently executed `tracing` call in the post-fork child.
                child_diag(&[
                    b"skipping bind mount, source does not exist: ",
                    bind_mount.source.as_os_str().as_bytes(),
                ]);
                continue;
            }

            let target = root.join(
                bind_mount
                    .target
                    .strip_prefix("/")
                    .unwrap_or(&bind_mount.target),
            );
            if build_mounts.contains(index) || bind_mount.source.is_dir() {
                fs::create_dir_all(&target)?;
            } else {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                if !target.exists() {
                    File::create(&target)?;
                }
            }

            if build_mounts.attach(index, &target)? {
                continue;
            }

            mount::<Path, Path, str, str>(
                Some(&bind_mount.source),
                &target,
                None,
                MsFlags::MS_BIND,
                None,
            )
            .map_err(|e| {
                let mut digits = int_buffer();
                child_diag(&[
                    b"bind mount ",
                    bind_mount.source.as_os_str().as_bytes(),
                    b" -> ",
                    target.as_os_str().as_bytes(),
                    b" failed: errno ",
                    format_int(&mut digits, e as i64),
                ]);
                sandbox_error(format!("Bind mount failed: {e}"))
            })?;

            if !bind_mount.writable
                && let Err(error) = set_mount_readonly(&target)
            {
                if bind_mount.target == Path::new("/etc/resolv.conf")
                    && self.try_fallback_readonly_copy(&bind_mount.source, &target)?
                {
                    continue;
                }
                self.handle_readonly_remount_failure(&target, error)?;
            }
        }

        self.chroot_into(root)
    }

    pub(in crate::container) fn try_fallback_readonly_copy(
        &self,
        source: &Path,
        target: &Path,
    ) -> Result<bool> {
        match umount2(target, MntFlags::MNT_DETACH) {
            Ok(()) | Err(nix::errno::Errno::EINVAL) => {}
            Err(error) => {
                return Err(sandbox_error(format!(
                    "failed to detach bind mount for {}: {error}",
                    target.display()
                )));
            }
        }

        fs::copy(source, target).map_err(|error| {
            sandbox_error(format!(
                "failed to copy {} into sandbox: {error}",
                source.display()
            ))
        })?;

        let mut permissions = fs::metadata(target)?.permissions();
        permissions.set_mode(0o444);
        fs::set_permissions(target, permissions)?;
        child_diag(&[
            b"falling back to copied read-only ",
            target.as_os_str().as_bytes(),
            b" inside sandbox",
        ]);
        Ok(true)
    }

    pub(in crate::container) fn handle_readonly_remount_failure(
        &self,
        target: &Path,
        error: nix::errno::Errno,
    ) -> Result<()> {
        let message = format!("read-only remount failed for {}: {error}", target.display());
        Err(execution_error(
            ScriptletFailureKind::EnforcementSetupFailed,
            message,
        ))
    }

    fn chroot_into(&self, root: &Path) -> Result<()> {
        let root_string = root.to_string_lossy().into_owned();
        let root_cstr = CString::new(root_string)
            .map_err(|e| sandbox_error(format!("Invalid root path: {e}")))?;
        chroot_syscall(&root_cstr).map_err(|e| sandbox_error(format!("chroot failed: {e}")))?;
        chdir_syscall(c"/")
            .map_err(|e| sandbox_error(format!("chdir after chroot failed: {e}")))?;
        Ok(())
    }

    pub(super) fn apply_resource_limits(&self) -> Result<()> {
        set_rlimit(libc::RLIMIT_AS, self.config.memory_limit, "RLIMIT_AS");
        set_rlimit(libc::RLIMIT_CPU, self.config.cpu_time_limit, "RLIMIT_CPU");
        set_rlimit(
            libc::RLIMIT_FSIZE,
            self.config.file_size_limit,
            "RLIMIT_FSIZE",
        );
        set_rlimit(libc::RLIMIT_NPROC, self.config.nproc_limit, "RLIMIT_NPROC");
        Ok(())
    }
}

fn wait_for_pid_namespace_init(child: Pid, deadline: Instant) -> Result<i32> {
    match wait_for_child_until(child, deadline) {
        Ok(ChildWaitOutcome::Exited(code)) => Ok(code),
        Ok(ChildWaitOutcome::Signaled(signal)) => Ok(128 + signal as i32),
        Ok(ChildWaitOutcome::TimedOut) => Err(execution_error(
            ScriptletFailureKind::ScriptTimedOut,
            "PID namespace init timed out",
        )),
        Err(error) => Err(sandbox_error(format!(
            "failed to wait for PID namespace init: {error}"
        ))),
    }
}

/// Set only the requested restriction. A legacy remount without inherited
/// flags tries to clear locked nosuid/nodev/noexec flags in a user namespace.
fn set_mount_readonly(target: &Path) -> std::result::Result<(), nix::errno::Errno> {
    let target =
        CString::new(target.as_os_str().as_bytes()).map_err(|_| nix::errno::Errno::EINVAL)?;
    let attr = libc::mount_attr {
        attr_set: libc::MOUNT_ATTR_RDONLY,
        attr_clr: 0,
        propagation: 0,
        userns_fd: 0,
    };
    // SAFETY: target and the initialized mount attribute remain valid for the
    // synchronous syscall; no mount restrictions are cleared.
    if unsafe {
        libc::syscall(
            libc::SYS_mount_setattr,
            libc::AT_FDCWD,
            target.as_ptr(),
            0,
            &attr,
            std::mem::size_of::<libc::mount_attr>(),
        )
    } < 0
    {
        Err(nix::errno::Errno::last())
    } else {
        Ok(())
    }
}

/// Create only sandbox-owned mount-point parents before any binds are attached.
/// Explicit modes make namespace traversal independent of the caller's umask.
fn create_mount_parents(root: &Path, directory: &Path) -> Result<()> {
    let mut current = root.to_path_buf();
    for component in directory
        .strip_prefix(root)
        .map_err(|error| sandbox_error(error.to_string()))?
        .components()
    {
        current.push(component);
        match fs::create_dir(&current) {
            Ok(()) => fs::set_permissions(&current, fs::Permissions::from_mode(0o755))?,
            Err(error)
                if error.kind() == std::io::ErrorKind::AlreadyExists
                    && fs::symlink_metadata(&current)?.is_dir() => {}
            Err(error) => {
                return Err(sandbox_error(format!(
                    "Cannot prepare mount directory: {error}"
                )));
            }
        }
    }
    Ok(())
}
