// apps/conary/tests/cli_ccs_build.rs

#![cfg(test)]
//! Actual authoring commands retain typed build facts and distinguish planned output.

use conary_core::ccs::CcsManifest;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fixture {
    _temp: tempfile::TempDir,
    project: PathBuf,
    source: PathBuf,
    output: PathBuf,
    data: PathBuf,
    config: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project with spaces");
        let source = temp.path().join("payload");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(source.join("bin")).unwrap();
        std::fs::write(source.join("bin/hello"), b"fixture\n").unwrap();
        let mut manifest = CcsManifest::new_minimal("summary-build", "2.0.0");
        manifest.package.release = "7".into();
        std::fs::write(project.join("ccs.toml"), manifest.to_toml().unwrap()).unwrap();
        Self {
            project,
            source,
            output: temp.path().join("output with spaces"),
            data: temp.path().join("data"),
            config: temp.path().join("config"),
            _temp: temp,
        }
    }

    fn capture(&self, tty: bool, no_color: bool, dry_run: bool, chunked: bool) -> Output {
        let mut command = if tty {
            let mut command = Command::new("script");
            // Paths are environment values, never interpolated into shell source.
            let mut script = String::from(
                "exec \"$CONARY_BUILD_EXE\" ccs build \"$CONARY_BUILD_PROJECT\" --source \"$CONARY_BUILD_SOURCE\" --output \"$CONARY_BUILD_OUTPUT\" --local-dev",
            );
            if dry_run {
                script.push_str(" --dry-run");
            }
            if !chunked {
                script.push_str(" --no-chunked");
            }
            command.args(["-qec", &script, "/dev/null"]);
            command
                .env("CONARY_BUILD_EXE", env!("CARGO_BIN_EXE_conary"))
                .env("CONARY_BUILD_PROJECT", &self.project)
                .env("CONARY_BUILD_SOURCE", &self.source)
                .env("CONARY_BUILD_OUTPUT", &self.output);
            command
        } else {
            let mut command = Command::new(env!("CARGO_BIN_EXE_conary"));
            command
                .args(["ccs", "build"])
                .arg(&self.project)
                .arg("--source")
                .arg(&self.source)
                .arg("--output")
                .arg(&self.output)
                .arg("--local-dev");
            if dry_run {
                command.arg("--dry-run");
            }
            if !chunked {
                command.arg("--no-chunked");
            }
            command
        };
        command
            .env("XDG_DATA_HOME", &self.data)
            .env("XDG_CONFIG_HOME", &self.config)
            .env("TERM", "xterm")
            .env_remove("NO_COLOR")
            .env_remove("CLICOLOR_FORCE");
        if no_color {
            command.env("NO_COLOR", "1");
        }
        command
            .output()
            .expect("capture CCS build (requires util-linux script)")
    }
}

fn captured_text(output: &Output, tty: bool, no_color: bool) -> String {
    let raw = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .replace("\r\n", "\n");
    if tty && !no_color {
        assert!(
            raw.contains('\x1b'),
            "terminal styling was not exercised: {raw}"
        );
    } else {
        assert!(!raw.contains('\x1b'), "unexpected escape sequence: {raw:?}");
    }
    console::strip_ansi_codes(&raw).into_owned()
}

fn inspected_file_count(artifact: &Path) -> usize {
    let output = Command::new(env!("CARGO_BIN_EXE_conary"))
        .args(["ccs", "inspect"])
        .arg(artifact)
        .args(["--files", "--format", "json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let inspection: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    inspection["files"].as_array().unwrap().len()
}

#[test]
fn build_and_preview_frames_in_terminal_pipe_and_no_color() {
    for (tty, no_color) in [(false, false), (true, false), (true, true)] {
        for (dry_run, chunked) in [(true, true), (false, true), (false, false)] {
            let fixture = Fixture::new();
            let output = fixture.capture(tty, no_color, dry_run, chunked);
            let frame = captured_text(&output, tty, no_color);
            assert!(output.status.success(), "{frame}");
            for field in ["Package: summary-build", "Version: 2.0.0", "CCS release: 7"] {
                assert_eq!(frame.matches(field).count(), 1, "{frame}");
            }
            assert!(!frame.contains("v2.0.0"), "{frame}");
            let artifact = fixture.output.join("summary-build-2.0.0-7.ccs");
            assert!(frame.contains(&artifact.display().to_string()), "{frame}");
            if dry_run {
                assert!(frame.contains("Planned package build:"), "{frame}");
                assert!(frame.contains("Planned artifacts:"), "{frame}");
                assert!(
                    frame.contains("Dry run: no package artifacts were written."),
                    "{frame}"
                );
                assert!(!frame.contains("Package build summary:"), "{frame}");
                assert!(!frame.contains("Created:"), "{frame}");
                assert!(!frame.contains("Built summary-build"), "{frame}");
                assert!(
                    !fixture.output.exists(),
                    "preview created its output directory"
                );
                assert!(!fixture.data.exists(), "preview initialized a signing key");
            } else {
                assert_eq!(
                    frame.matches("Package build summary:").count(),
                    1,
                    "{frame}"
                );
                assert_eq!(frame.matches("Created:").count(), 1, "{frame}");
                assert_eq!(frame.matches("Built summary-build").count(), 1, "{frame}");
                assert!(frame.contains("Architecture: noarch"), "{frame}");
                assert!(frame.contains("Payload size: 8 bytes"), "{frame}");
                assert!(frame.contains("Payload sources: 1 regular file"), "{frame}");
                assert!(frame.contains("Components:"), "{frame}");
                assert!(frame.contains("local-dev CCS key"), "{frame}");
                assert!(artifact.is_file());
                let file_count = inspected_file_count(&artifact);
                assert!(
                    frame.contains(&format!("File records: {file_count}")),
                    "{frame}"
                );
                assert_eq!(frame.contains("Total chunks:"), chunked, "{frame}");
                assert!(!frame.contains("Dry run:"), "{frame}");
            }
        }
    }
}

#[test]
fn missing_manifest_names_the_current_init_command() {
    let fixture = Fixture::new();
    std::fs::remove_file(fixture.project.join("ccs.toml")).unwrap();
    let output = fixture.capture(false, true, false, true);
    assert!(!output.status.success());
    let frame = captured_text(&output, false, true);
    assert!(frame.contains("Run 'conary ccs init' first."), "{frame}");
    assert!(!fixture.output.exists());
    assert!(!fixture.data.exists());
}
