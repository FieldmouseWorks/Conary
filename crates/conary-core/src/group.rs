//! A child started as the leader of a process group of its own, so that a
//! deadline ends whatever it started too. Killing only the child leaves a
//! descendant it was waiting on running, free to finish what the caller
//! already took for not done.
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};

/// Starts `command` in a new process group it leads.
pub fn spawn(command: &mut Command) -> std::io::Result<Child> {
    command.process_group(0).spawn()
}

/// Kills every process of the group `child` leads, and reaps `child`.
pub fn kill(child: &mut Child) {
    let group = rustix::process::Pid::from_child(child);
    let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// A wrapper waiting on a command it started: killed as a group, the
    /// command never gets to write its marker.
    #[test]
    fn a_killed_group_takes_its_descendants_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("marker");
        let mut child = spawn(
            Command::new("/bin/sh")
                .arg("-c")
                .arg(r#"sh -c 'sleep 1; touch "$0"' "$0"; true"#)
                .arg(&marker),
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        kill(&mut child);
        let until = Instant::now() + Duration::from_secs(2);
        while Instant::now() < until {
            assert!(!marker.exists(), "a descendant outlived its group's kill");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
