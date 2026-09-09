// crates/conary-core/src/container/execution/monitor.rs

//! Pin the PID-namespace monitor across credential changes and parent death.

use super::sandbox_error;
use crate::container::namespaces::set_parent_death_signal;
use crate::error::Result;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

pub(super) struct PidNamespaceMonitor(OwnedFd);

impl PidNamespaceMonitor {
    /// Open in the monitor before fork: its PID is not visible from namespace init.
    pub(super) fn current() -> Result<Self> {
        Self::open(unsafe { libc::getpid() })
    }

    fn open(pid: libc::pid_t) -> Result<Self> {
        // SAFETY: pidfd_open takes integer arguments and returns a fresh CLOEXEC fd.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if fd < 0 {
            return Err(sandbox_error(format!(
                "Cannot pin PID namespace monitor: {}",
                std::io::Error::last_os_error()
            )));
        }
        // SAFETY: fd is newly owned on successful pidfd_open.
        Ok(Self(unsafe { OwnedFd::from_raw_fd(fd as i32) }))
    }

    /// Credential changes clear PDEATHSIG. Arm it, then close the already-dead
    /// parent race using the retained pidfd; getppid is always zero for this init.
    pub(super) fn bind(&self) -> Result<()> {
        set_parent_death_signal(libc::SIGKILL).map_err(|error| {
            sandbox_error(format!(
                "Cannot bind PID namespace monitor lifetime: {error}"
            ))
        })?;
        self.ensure_alive()
    }

    fn ensure_alive(&self) -> Result<()> {
        let mut poll = libc::pollfd {
            fd: self.0.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll points to one initialized entry; timeout zero cannot block.
        let result = unsafe { libc::poll(&mut poll, 1, 0) };
        if result < 0 {
            return Err(sandbox_error(format!(
                "Cannot inspect PID namespace monitor lifetime: {}",
                std::io::Error::last_os_error()
            )));
        }
        if result != 0 {
            return Err(sandbox_error("PID namespace monitor exited during setup"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_monitor_is_alive_and_descriptor_is_close_on_exec() {
        let monitor = PidNamespaceMonitor::current().unwrap();
        monitor.ensure_alive().unwrap();
        let flags = unsafe { libc::fcntl(monitor.0.as_raw_fd(), libc::F_GETFD) };
        assert!(flags >= 0);
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
    }

    #[test]
    fn exited_monitor_is_rejected_after_reaping() {
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("60")
            .spawn()
            .unwrap();
        let monitor = PidNamespaceMonitor::open(child.id() as libc::pid_t);
        child.kill().unwrap();
        child.wait().unwrap();
        let error = monitor.unwrap().ensure_alive().unwrap_err();
        assert!(error.to_string().contains("monitor exited during setup"));
    }
}
