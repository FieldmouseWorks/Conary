//! A worker invokes the negotiated runner. A pass only starts or collects it.
use super::persist::{self, Result};
use super::*;
use conary_core::plugin::{self, Limits, Plugin, Task};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Purpose {
    Review(String),
    Fix(Kind),
    Judge,
    Proof,
    /// A proof's preparation, which holds no proof slot.
    Prepare,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Work {
    pub purpose: Purpose,
    pub job: Job,
    pub path: PathBuf,
    pub deadline: u64,
    pub posted: bool,
    pub recipes: std::collections::BTreeMap<String, Recipe>,
    /// A proof its preparation said needs no proof slot, so holds none.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub slotless: bool,
    /// A fix's problems, by index: the finding each was and the target its
    /// answer goes to, or None for a problem that isn't a finding.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub handed: Vec<Option<Handed>>,
    /// What publishing this job's answer has sent so far that GitHub
    /// acknowledged, saved as it goes: a later attempt waits to read it
    /// back, and never sends it again.
    #[serde(default, skip_serializing_if = "Sent::is_empty")]
    pub sent: Sent,
}

/// A publication's progress. For a review: its plan, which findings get a
/// line comment, fixed before the review is first sent. For a review or a
/// fix: each of its writes, by what it is ("review", "finding 2",
/// "answer 0"), saved before it's sent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline: Option<Vec<bool>>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub writes: std::collections::BTreeMap<String, Attempt>,
}

/// One write's sending: when it was last sent, how many times, and whether
/// GitHub acknowledged it. One not acknowledged may or may not have gone up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attempt {
    pub at: u64,
    pub tries: u32,
    pub acknowledged: bool,
}
impl Sent {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Invocation {
    pub executable: PathBuf,
    pub task: Task,
    #[serde(default)]
    pub choice: Option<plugin::Choice>,
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
            .call_with(
                invocation.task,
                invocation.choice,
                plugin::Context::Fresh,
                Some(&path.with_extension("cancel")),
            )
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
/// Told when a worker exits, however it ended.
pub type Ended = std::sync::Arc<dyn Fn() + Send + Sync>;
/// The CLI's hidden worker entry, with file-backed output and no inherited pipes.
pub struct Process {
    pub worker: PathBuf,
    /// Told once each worker this launches has exited, answered or not: only
    /// a pass reads its answer, and most workers publish nothing GitHub sends
    /// an event for, so this is how a daemon reads it without waiting on a
    /// deadline.
    pub ended: Option<Ended>,
}
impl Launch for Process {
    fn launch(&self, path: &Path) -> Result<()> {
        let log = std::fs::File::create(path.with_extension("log"))?;
        // Locked before the worker exists, and held by it as its standard
        // input, which it never reads, until it exits: so a daemon that
        // didn't launch it still sees it end (`watch`).
        let running = running(path)?;
        running.try_lock()?;
        let mut child = Command::new(&self.worker)
            .arg("flow-worker")
            .arg(path)
            .stdin(running)
            .stdout(log.try_clone()?)
            .stderr(log)
            .spawn()?;
        // Reap fast workers while a large pass is still running. Rust does
        // not wait for detached threads when the invoking CLI exits.
        let ended = self.ended.clone();
        std::thread::spawn(move || {
            let _ = child.wait();
            if let Some(ended) = ended {
                ended();
            }
        });
        Ok(())
    }
}
/// The lock a launched worker holds from before it starts until it exits.
fn running(path: &Path) -> Result<std::fs::File> {
    Ok(std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path.with_extension("running"))?)
}
/// Tells `ended` once the worker launched for `path` has exited, however it
/// ended, from a process that didn't launch it: a daemon started while its
/// predecessor's workers still run. A worker that already exited, or never
/// started, holds nothing, so `ended` hears of it at once.
pub fn watch(path: &Path, ended: Ended) -> Result<()> {
    let running = running(path)?;
    std::thread::Builder::new().spawn(move || {
        let _ = running.lock();
        let _ = running.unlock();
        ended();
    })?;
    Ok(())
}
/// The invocations saved in a repository's flow directory that have no
/// answer yet: those whose workers may still be running.
pub fn outstanding(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        entries => entries?,
    };
    let mut jobs = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "job") && !path.with_extension("answer").exists() {
            jobs.push(path);
        }
    }
    jobs.sort();
    Ok(jobs)
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
