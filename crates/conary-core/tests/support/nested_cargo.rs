//! The one way a test runs a cargo build, shared by the packages that do.
//!
//! The outer `cargo test` hands each test its package's `CARGO_*` variables,
//! and build scripts that watch those (ring's) rebuild under them. In the
//! outer run's directory that replaces the libraries it has still to hand to
//! rustdoc, so a test's cargo builds in a directory of its own.
use std::path::Path;
use std::process::Command;

pub fn cargo() -> Command {
    let cargo = || {
        let mut command = Command::new(env!("CARGO"));
        command.current_dir(env!("CARGO_MANIFEST_DIR"));
        command
    };
    let out = cargo()
        .args(["metadata", "--no-deps", "--offline", "--format-version=1"])
        .output()
        .expect("cargo metadata must run");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let metadata: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let target = metadata["target_directory"].as_str().unwrap();
    let nested = Path::new(target).join("nested-cargo");
    let mut command = cargo();
    // Both: a configured build directory doesn't follow the target directory.
    command
        .env("CARGO_TARGET_DIR", &nested)
        .env("CARGO_BUILD_BUILD_DIR", &nested);
    command
}
