//! A worker invokes the unchanged v1 runner. A pass only starts or collects it.
use super::persist::{self, Result};
use super::*;
use conary_core::plugin::{self, Limits, Plugin, Task};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Purpose {
    Review(String),
    Fix(Kind),
    Judge,
    Proof,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Work {
    pub purpose: Purpose,
    pub generation: u64,
    pub job: Job,
    pub path: PathBuf,
    pub deadline: u64,
    pub posted: bool,
    pub recipes: std::collections::BTreeMap<String, Recipe>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Invocation {
    pub executable: PathBuf,
    pub task: Task,
    pub deadline: u64,
    pub response_bytes: usize,
}
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Completion {
    pub at: u64,
    pub result: Option<std::result::Result<plugin::Response, String>>,
}
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Workers lock their own invocation: duplicate launches never run it twice.
pub fn worker(path: &Path) -> Result<()> {
    let _lock = persist::LocalLock::take(&path.with_extension("lock"))?;
    if path.with_extension("answer").exists() {
        return Ok(());
    }
    let invocation: Invocation = serde_json::from_slice(&std::fs::read(path)?)?;
    let result = (|| {
        let remaining = invocation.deadline.saturating_sub(now());
        if remaining == 0 {
            return Err("plugin exceeded deadline".into());
        }
        let limits = Limits {
            deadline: Duration::from_secs(remaining),
            response_bytes: invocation.response_bytes,
        };
        let plugin = Plugin::describe_cancellable(
            &invocation.executable,
            limits,
            Some(&path.with_extension("cancel")),
        )
        .map_err(|e| e.to_string())?;
        let remaining = invocation.deadline.saturating_sub(now());
        if remaining == 0 {
            return Err("plugin exceeded deadline".into());
        }
        // Describe and call share an absolute deadline.
        let plugin = plugin.with_deadline(Duration::from_secs(remaining));
        plugin
            .call_cancellable(invocation.task, Some(&path.with_extension("cancel")))
            .map_err(|e| e.to_string())
    })();
    persist::save_once(
        &path.with_extension("answer"),
        &Completion {
            at: now(),
            result: Some(result),
        },
    )
}

pub trait Launch {
    fn launch(&self, path: &Path) -> Result<()>;
}
/// The CLI's hidden worker entry, with file-backed output and no inherited pipes.
pub struct Process(pub PathBuf);
impl Launch for Process {
    fn launch(&self, path: &Path) -> Result<()> {
        let log = std::fs::File::create(path.with_extension("log"))?;
        let mut child = Command::new(&self.0)
            .arg("flow-worker")
            .arg(path)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .spawn()?;
        // Reap fast workers while a large pass is still running. Rust does
        // not wait for detached threads when the invoking CLI exits.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }
}
pub fn completion(work: &Work, now: u64) -> Result<Completion> {
    collect(work, now, true)
}
pub fn inspect(work: &Work, now: u64) -> Result<Completion> {
    collect(work, now, false)
}
fn collect(work: &Work, now: u64, mutate: bool) -> Result<Completion> {
    let answer: Completion = persist::load(&work.path.with_extension("answer"))?;
    if (answer.result.is_none() && now >= work.deadline) || answer.at > work.deadline {
        let timed_out = Completion {
            at: work.deadline,
            result: Some(Err("plugin exceeded deadline or worker was lost".into())),
        };
        if mutate {
            std::fs::write(work.path.with_extension("cancel"), b"deadline\n")?;
            persist::save_once(&work.path.with_extension("answer"), &timed_out)?;
        }
        return Ok(timed_out);
    }
    Ok(answer)
}
pub fn cancel(work: &Work) -> Result<()> {
    std::fs::write(work.path.with_extension("cancel"), b"obsolete\n")?;
    Ok(())
}
