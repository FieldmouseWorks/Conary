//! `cn flow pause` and `cn flow resume` write and lift a repository's pause
//! in the flow's data directory alone: no daemon, GitHub, token,
//! configuration or deploy, and not kept off by a pass that holds the
//! repository's lock.
use std::process::{Command, Output};

fn cn(data: &std::path::Path, args: &[&str]) -> Output {
    let output = Command::new(env!("CARGO_BIN_EXE_cn"))
        .arg("flow")
        .args(args)
        .arg("--data")
        .arg(data)
        .env_clear()
        .env("GITHUB_API_URL", "http://127.0.0.1:1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
fn pause_and_resume_need_nothing_but_the_data_directory() {
    let data = tempfile::tempdir().unwrap();
    let dir = data.path().join("flow/owner/repo");
    std::fs::create_dir_all(&dir).unwrap();
    // A pass in progress holds the repository's lock throughout.
    let lock = std::fs::File::create(dir.join("lock")).unwrap();
    lock.try_lock().unwrap();

    let printed = cn(
        data.path(),
        &["pause", "Owner/Repo", "--reason", "moving the machine"],
    );
    let printed: serde_json::Value = serde_json::from_slice(&printed.stdout).unwrap();
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("pause")).unwrap()).unwrap();
    assert_eq!(printed, saved);
    assert_eq!(saved["scope"], "repository");
    assert_eq!(saved["reason"], "moving the machine");

    cn(data.path(), &["resume", "Owner/Repo"]);
    assert!(!dir.join("pause").exists());
    // Resuming what isn't paused is allowed too.
    cn(data.path(), &["resume", "Owner/Repo"]);
}
