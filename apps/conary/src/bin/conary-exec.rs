// apps/conary/src/bin/conary-exec.rs
//! `/usr/libexec/conary/conary-exec`: run a command from a source-root launch
//! tree as the calling user. Deliberately separate from `conary` so the
//! AppArmor `userns` grant stays narrow; it never opens a database and never
//! starts a thread (`unshare(CLONE_NEWUSER)` refuses multithreaded callers).

fn main() {
    std::process::exit(conary::exec_launcher::run());
}
