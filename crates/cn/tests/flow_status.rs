//! `cn flow status` answers from the data directory and the configuration
//! alone: no daemon, GitHub or token, and not kept off by a pass that holds
//! the repository's lock.
use std::process::Command;

#[test]
fn status_needs_nothing_but_the_data_directory_and_configuration() {
    let data = tempfile::tempdir().unwrap();
    let dir = data.path().join("flow/owner/repo");
    std::fs::create_dir_all(&dir).unwrap();
    // A pass in progress holds the repository's lock throughout.
    let lock = std::fs::File::create(dir.join("lock")).unwrap();
    lock.try_lock().unwrap();
    std::fs::write(
        dir.join("pause"),
        r#"{"scope":"repository","reason":"moving the machine","at":7}"#,
    )
    .unwrap();
    let config = data.path().join("flow.json");
    std::fs::write(&config, r#"{"plugins":{"fix":"/bin/true"}}"#).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_cn"))
        .args(["flow", "status", "Owner/Repo", "--data"])
        .arg(data.path())
        .arg("--config")
        .arg(&config)
        .env_clear()
        .env("GITHUB_API_URL", "http://127.0.0.1:1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    // No pass has read the open PRs yet, so the switches hold none.
    assert_eq!(status["read_at"], serde_json::Value::Null);
    let switches: Vec<_> = status["switches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            assert_eq!(s["prs"], serde_json::json!([]));
            (
                s["switch"].as_str().unwrap().to_string(),
                s["kind"].as_str().map(String::from),
            )
        })
        .collect();
    let named = |s: &str, kind: Option<&str>| (s.to_string(), kind.map(String::from));
    assert_eq!(
        switches,
        [
            named("pause", None),
            named("no_plugin", Some("review")),
            named("no_plugin", Some("judge")),
            named("no_plugin", Some("proof")),
            named("train_off", None),
            named("no_people", None),
            named("no_evidence", None),
        ]
    );
    assert_eq!(status["switches"][0]["reason"], "moving the machine");
    assert_eq!(status["switches"][0]["at"], 7);
}
