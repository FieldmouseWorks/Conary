//! End-to-end contract checks. These tests compile on the pre-plugin base:
//! only the existing cn executable and std/tempfile are used here.
use std::path::Path;
use std::process::{Command, Output};
use std::sync::OnceLock;

fn check(path: &Path, deadline: &str, cap: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cn"))
        .args(["plugin", "check"])
        .arg(path)
        .args(["--deadline-ms", deadline, "--response-bytes", cap])
        .output()
        .unwrap()
}
fn fixture(mode: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    static COMPILED: OnceLock<tempfile::TempDir> = OnceLock::new();
    let compiled = COMPILED.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let status = Command::new("rustc")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/plugin_fixture.rs"))
            .arg("-o")
            .arg(dir.path().join("fixture"))
            .status()
            .unwrap();
        assert!(status.success());
        dir
    });
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(mode);
    // The binary is immutable after compilation. A hard link gives each mode
    // its own executable name without opening it for writing during parallel
    // spawns (a child can inherit a writer briefly and cause ETXTBSY).
    std::fs::hard_link(compiled.path().join("fixture"), &path).unwrap();
    (dir, path)
}
#[test]
fn plugin_example_conforms() {
    // Cargo builds all package binaries for integration tests; locating this
    // beside cn also keeps this test compilable on the pre-plugin base.
    let path = Path::new(env!("CARGO_BIN_EXE_cn")).with_file_name("conary-example-plugin");
    let out = check(&path, "5000", "65536");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        value["checked"],
        serde_json::json!(["review", "fix", "judge", "proof", "job"])
    );
}
#[test]
fn plugin_accepts_an_independent_session_of_the_same_engine() {
    let (_dir, path) = fixture("valid");
    let out = check(&path, "5000", "4096");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
fn rejects(mode: &str, diagnostic: &str) {
    let (_dir, path) = fixture(mode);
    let out = check(
        &path,
        if mode == "deadline" { "25" } else { "5000" },
        "4096",
    );
    assert!(!out.status.success(), "accepted broken plugin: {mode}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains(diagnostic),
        "{mode}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
#[test]
fn plugin_rejects_wrong_version() {
    rejects("version", "wrong plugin contract");
}
#[test]
fn plugin_rejects_malformed_json() {
    rejects("malformed", "invalid plugin JSON");
}
#[test]
fn plugin_rejects_missing_field() {
    rejects("missing", "invalid plugin JSON");
}
#[test]
fn plugin_rejects_undescribed_kind() {
    rejects("undescribed", "response kind does not match");
}
#[test]
fn plugin_rejects_large_response() {
    rejects("large", "response too large");
}
#[test]
fn plugin_rejects_deadline() {
    rejects("deadline", "exceeded deadline");
}
#[test]
fn plugin_rejects_nonzero_exit() {
    rejects("exit", "exited unsuccessfully");
}
#[test]
fn plugin_rejects_author_grading() {
    rejects("author", "author cannot grade");
}

#[test]
fn plugin_rejects_author_switching_engines() {
    rejects("cross-author", "author cannot grade");
}
#[test]
fn plugin_rejects_unknown_session_of_author_engine() {
    rejects("unknown-session", "author cannot grade");
}

#[test]
fn plugin_rejects_absolute_finding_path() {
    rejects("absolute-path", "invalid finding");
}
#[test]
fn plugin_rejects_parent_finding_path() {
    rejects("parent-path", "invalid finding");
}
#[test]
fn plugin_rejects_nested_finding_path_escape() {
    rejects("nested-escape", "invalid finding");
}
#[test]
fn plugin_cli_preserves_default_cargo_run() {
    let out = Command::new(env!("CARGO"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args([
            "run",
            "--quiet",
            "--offline",
            "--locked",
            "-p",
            "conary-cli",
            "--",
            "--help",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("Conary: the ledger"));
}

#[test]
fn cargo_manifest_preserves_cli_and_example_targets() {
    // The example plugin is a second binary of this package; without a
    // default, `cargo run -p conary-cli` (README) cannot pick one.
    let out = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args([
            "metadata",
            "--no-deps",
            "--offline",
            "--format-version",
            "1",
        ])
        .arg("--manifest-path")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let metadata: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let package = metadata["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "conary-cli")
        .unwrap();
    assert_eq!(package["default_run"], "cn");
    let bins: Vec<_> = package["targets"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["kind"] == serde_json::json!(["bin"]))
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(bins.contains(&"conary-example-plugin"), "{bins:?}");
}

#[test]
fn plugin_rejects_finding_path_escaping_the_repository() {
    rejects("escape", "invalid finding");
}

#[test]
fn plugin_accepts_v2_declared_offers_and_a_job() {
    let (_dir, path) = fixture("v2-valid");
    let out = check(&path, "5000", "4096");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["checked"], serde_json::json!(["review", "job"]));
}
#[test]
fn plugin_rejects_a_model_advertising_proof() {
    rejects("v2-proof-model", "invalid declared models");
}
#[test]
fn plugin_rejects_v2_without_capabilities() {
    rejects("v2-no-caps", "must declare its capabilities");
}
#[test]
fn plugin_rejects_duplicate_models() {
    rejects("v2-dup-model", "invalid declared models");
}
#[test]
fn plugin_rejects_job_under_v1() {
    rejects("v1-job", "needs conary-plugin v2");
}
#[test]
fn plugin_rejects_job_with_empty_summary() {
    rejects("v2-job-empty", "does not match request or required reason");
}
#[test]
fn plugin_rejects_job_ask_without_default() {
    rejects(
        "v2-job-bad-ask",
        "does not match request or required reason",
    );
}
#[test]
fn plugin_job_fixture_must_end_done() {
    rejects("v2-job-await", "conformance fixture");
}
