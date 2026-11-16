use conary_core::plugin::{Failure, Limits, Plugin};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

pub struct Scripted {
    _dir: tempfile::TempDir,
    pub path: PathBuf,
}

/// A plugin that describes itself as `describe` and gives `answers`, in order.
pub fn scripted(describe: Value, answers: &[Value]) -> Scripted {
    static COMPILED: OnceLock<tempfile::TempDir> = OnceLock::new();
    let compiled = COMPILED.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let status = Command::new("rustc")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/scripted_plugin.rs"))
            .arg("-o")
            .arg(dir.path().join("plugin"))
            .status()
            .unwrap();
        assert!(status.success());
        dir
    });
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plugin");
    // A hard link: never a file open for writing while another test spawns.
    std::fs::hard_link(compiled.path().join("plugin"), &path).unwrap();
    std::fs::write(path.with_extension("describe"), describe.to_string()).unwrap();
    let lines: Vec<_> = answers.iter().map(Value::to_string).collect();
    std::fs::write(path.with_extension("answers"), lines.join("\n")).unwrap();
    Scripted { _dir: dir, path }
}

impl Scripted {
    pub fn plugin(&self) -> Result<Plugin, Failure> {
        Plugin::describe(
            &self.path,
            Limits {
                deadline: Duration::from_secs(10),
                response_bytes: 65_536,
            },
        )
    }
    /// The work requests the plugin received, in order.
    pub fn requests(&self) -> Vec<Value> {
        std::fs::read_to_string(self.path.with_extension("requests"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}
