//! `cn flow settings --schema` prints the schema whatever configuration
//! the environment names, reading none of it; given a file, it lists the
//! file's options.
use std::process::Command;

#[test]
fn the_schema_prints_with_every_configuration_named_in_the_environment() {
    let output = Command::new(env!("CARGO_BIN_EXE_cn"))
        .args(["flow", "settings", "--schema"])
        .env("CONARY_FLOW_CONFIG", "/nonexistent/flow.json")
        .env("CONARY_EVENTS_CONFIG", "/nonexistent/events.json")
        .env("CONARY_GATE_REUSE_CONFIG", "/nonexistent/gate-reuse.json")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let schema: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(schema["flow"]["title"], "Config");
    assert_eq!(schema["events"]["title"], "Runtime");
    assert_eq!(schema["gate_reuse"]["title"], "Settings");
}

#[test]
fn the_gate_reuse_configuration_lists_its_options_and_listen_apart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gate-reuse.json");
    std::fs::write(
        &path,
        serde_json::json!({
            "listen": "127.0.0.1:8091",
            "repos": ["example/project"],
            "token_command": "conary-token --key secret",
            "signer": "example/ci/.github/workflows/gate.yml",
        })
        .to_string(),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_cn"))
        .args(["flow", "settings", "--gate-reuse-config"])
        .arg(&path)
        .env_remove("CONARY_FLOW_CONFIG")
        .env_remove("CONARY_EVENTS_CONFIG")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let printed = String::from_utf8(output.stdout).unwrap();
    assert!(!printed.contains("secret"), "{printed}");
    let listing: serde_json::Value = serde_json::from_str(&printed).unwrap();
    let names = |list: &str| -> Vec<String> {
        listing["gate_reuse"][list]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["name"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(
        names("options"),
        ["api", "fresh", "repos", "server", "signer", "token_command"]
    );
    assert_eq!(names("not_live"), ["listen"]);
    let api = &listing["gate_reuse"]["options"][0];
    assert_eq!(api["value"], "https://api.github.com");
    assert_eq!(api["from"], "default");
}
