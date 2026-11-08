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
    std::fs::copy(compiled.path().join("fixture"), &path).unwrap();
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
        serde_json::json!(["review", "fix", "judge", "proof"])
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
