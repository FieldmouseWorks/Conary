//! `cn flow settings --schema` prints the schema whatever configuration
//! the environment names, reading none of it.
use std::process::Command;

#[test]
fn the_schema_prints_with_both_configurations_named_in_the_environment() {
    let output = Command::new(env!("CARGO_BIN_EXE_cn"))
        .args(["flow", "settings", "--schema"])
        .env("CONARY_FLOW_CONFIG", "/nonexistent/flow.json")
        .env("CONARY_EVENTS_CONFIG", "/nonexistent/events.json")
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
}
