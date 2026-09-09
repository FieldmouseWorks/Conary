// crates/conary-core/src/container/execution/credentials.rs

//! Seal the namespace identity after filesystem setup and before exec.

use super::sandbox_error;
use crate::error::Result;

#[repr(C)]
struct CapabilityHeader {
    version: u32,
    pid: i32,
}

/// Remove setup capabilities, including their exec-time recovery paths. All
/// operations are direct syscalls suitable for the post-fork child.
pub(super) fn seal_namespace_credentials() -> Result<()> {
    // SAFETY: these prctl operations take integer arguments only.
    check(unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) })?;
    check(unsafe {
        libc::prctl(
            libc::PR_CAP_AMBIENT,
            libc::PR_CAP_AMBIENT_CLEAR_ALL,
            0,
            0,
            0,
        )
    })?;

    // Linux capability ABI v3 has two 32-bit words. Ask the kernel which
    // capability numbers exist; do not depend on a userspace enum's age.
    for capability in 0..64 {
        let supported = unsafe { libc::prctl(libc::PR_CAPBSET_READ, capability, 0, 0, 0) };
        if supported < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINVAL) {
            break;
        }
        check(supported)?;
        check(unsafe { libc::prctl(libc::PR_CAPBSET_DROP, capability, 0, 0, 0) })?;
    }
    let header = CapabilityHeader {
        version: 0x2008_0522, // _LINUX_CAPABILITY_VERSION_3
        pid: 0,
    };
    // Two __user_cap_data_struct entries: effective/permitted/inheritable.
    let data = [[0_u32; 3]; 2];
    // SAFETY: the header and two ABI-v3 entries are initialized and live for
    // this synchronous syscall; pid 0 targets only the forked calling thread.
    check(unsafe { libc::syscall(libc::SYS_capset, &header, &data) } as i32)
}

fn check(result: i32) -> Result<()> {
    if result < 0 {
        return Err(sandbox_error(format!(
            "Cannot seal sandbox credentials: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}
