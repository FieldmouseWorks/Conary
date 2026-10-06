// crates/conary-core/src/launch/exec.rs

//! Replace the launcher with the target. `execve` keeps the process, so the
//! terminal, signals, and exit status belong to the target with no
//! supervisor in between.

use std::ffi::{CString, OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use nix::errno::Errno;

use super::error::LaunchError;
use super::policy::exec_candidates;

/// The typed result of a `PATH` search in which no candidate executed.
/// Following `execvp`, a permission failure anywhere wins over "not found".
pub fn search_exhausted(command: &OsStr, permission_denied: bool) -> LaunchError {
    if permission_denied {
        LaunchError::TargetNotExecutable {
            command: command.to_os_string(),
        }
    } else {
        LaunchError::TargetNotFound {
            command: command.to_os_string(),
        }
    }
}

fn c_string(value: &OsStr) -> Result<CString, LaunchError> {
    CString::new(value.as_bytes()).map_err(|_| LaunchError::InvalidArgument(value.to_os_string()))
}

/// The caller's environment with `PWD` naming the directory inside the root.
fn environment(pwd: &Path) -> Result<Vec<CString>, LaunchError> {
    let mut env = Vec::new();
    for (key, value) in std::env::vars_os() {
        if key == "PWD" {
            continue;
        }
        let mut entry = OsString::from(&key);
        entry.push("=");
        entry.push(&value);
        env.push(c_string(&entry)?);
    }
    let mut pwd_entry = OsString::from("PWD=");
    pwd_entry.push(pwd);
    env.push(c_string(&pwd_entry)?);
    Ok(env)
}

/// Execute `command` inside the root. Returns only on failure.
pub(super) fn exec_target(command: &[OsString], pwd: &Path) -> LaunchError {
    let Some(program) = command.first() else {
        return LaunchError::MissingCommand;
    };
    let argv = match command
        .iter()
        .map(|arg| c_string(arg))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(argv) => argv,
        Err(error) => return error,
    };
    let env = match environment(pwd) {
        Ok(env) => env,
        Err(error) => return error,
    };
    let path_var = std::env::var_os("PATH");
    let mut permission_denied = false;
    for candidate in exec_candidates(program, path_var.as_deref()) {
        let path = match c_string(candidate.as_os_str()) {
            Ok(path) => path,
            Err(error) => return error,
        };
        let Err(errno) = nix::unistd::execve(&path, &argv, &env);
        match errno {
            Errno::ENOENT | Errno::ENOTDIR => {}
            Errno::EACCES => permission_denied = true,
            errno => {
                return LaunchError::ExecFailed {
                    path: candidate,
                    errno,
                };
            }
        }
    }
    search_exhausted(program, permission_denied)
}
