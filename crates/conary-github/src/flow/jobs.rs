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
    Fix(Kind),
    Judge,
    Proof,
    /// A proof's preparation, which holds no proof slot.
    Prepare,
}
impl Purpose {
    /// The plugin kind this work runs.
    pub fn kind(&self) -> conary_core::plugin::Kind {
        use conary_core::plugin::Kind;
        match self {
            Self::Fix(_) => Kind::Fix,
            Self::Judge => Kind::Judge,
            Self::Proof => Kind::Proof,
            Self::Prepare => Kind::Prepare,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Work {
    pub purpose: Purpose,
    pub job: Job,
    pub path: PathBuf,
    pub deadline: u64,
    pub posted: bool,
    pub recipes: std::collections::BTreeMap<String, Recipe>,
    /// The tree a proof or its preparation was asked to run on, which its
    /// deadlines are counted against. None for other work, and for work
    /// saved before the flow kept it, which counts toward no tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree: Option<String>,
    /// A proof its preparation said needs no proof slot, so holds none.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub slotless: bool,
    /// A fix's problems, by index: the review thread each was, which its
    /// answer is a reply on, or None for a problem that isn't a thread.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub handed: Vec<Option<String>>,
    /// What publishing this job's answer has sent so far that GitHub
    /// acknowledged, saved as it goes: a later attempt waits to read it
    /// back, and never sends it again.
    #[serde(default, skip_serializing_if = "Sent::is_empty")]
    pub sent: Sent,
}

/// A fix's publication progress: each of its writes, by what it is
/// ("answer 0"), saved before it's sent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sent {
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
impl Work {
    /// Whether this work's invocation reached its deadline: it ended
    /// without an answer by then, whether the worker said so, the plugin
    /// ran out its time, or the worker was lost and the pass said so for it
    /// ([`completion`]). Read from the completion alone, so a dry run
    /// that writes nothing tells it the same.
    pub fn reached_deadline(&self, completion: &Completion) -> bool {
        matches!(completion.result, Some(Err(_))) && completion.at >= self.deadline
    }
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
/// A worker's answer that's there and doesn't read, or answers another
/// kind of work, or a stop marker that doesn't say why: a degraded path
/// ([`super::degraded`]), told apart from a failure to act on one.
#[derive(Debug)]
pub struct Unreadable(pub persist::Error);
impl std::fmt::Display for Unreadable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the worker's answer doesn't read: {}", self.0)
    }
}
impl std::error::Error for Unreadable {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&*self.0)
    }
}
/// The worker's answer, read only with its stop marker: what became of it
/// is both.
fn collect(work: &Work, now: u64, mutate: bool) -> Result<Completion> {
    stopped(work)?;
    let answer: Completion =
        persist::load(&work.path.with_extension("answer")).map_err(Unreadable)?;
    // The runner refuses an answer to another kind of work, so one here is
    // an answer file that says something it never wrote.
    if let Some(Ok(response)) = &answer.result
        && response.answer.kind() != Some(work.purpose.kind())
    {
        let unparsed = super::degraded::Unparsed("it answers another kind of work");
        return Err(Unreadable(Box::new(unparsed)).into());
    }
    if (answer.result.is_none() && now >= work.deadline) || answer.at > work.deadline {
        let timed_out = Completion {
            at: work.deadline,
            result: Some(Err("plugin exceeded deadline or worker was lost".into())),
        };
        if mutate {
            mark(work, DEADLINE)?;
            persist::save_once(&work.path.with_extension("answer"), &timed_out)?;
        }
        return Ok(timed_out);
    }
    Ok(answer)
}
/// Ask the worker to stop, recording why. The first reason asked stays.
pub fn cancel(work: &Work, why: super::pause::Stop) -> Result<()> {
    mark(work, why.marker())
}
/// The marker a worker stopped at its deadline has.
const DEADLINE: &[u8] = b"deadline\n";
/// Why a worker was asked to stop, if it was asked as a stop rather than at
/// its deadline. A marker that can't be read, or that says neither, is
/// [`Unreadable`].
pub(crate) fn stopped(work: &Work) -> std::result::Result<Option<super::pause::Stop>, Unreadable> {
    let marker = match std::fs::read(work.path.with_extension("cancel")) {
        Ok(marker) => marker,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Unreadable(e.into())),
    };
    match super::pause::Stop::from_marker(&marker) {
        Some(stop) => Ok(Some(stop)),
        None if marker == DEADLINE => Ok(None),
        None => Err(Unreadable(Box::new(super::degraded::Unparsed(
            "its stop marker says neither a stop nor its deadline",
        )))),
    }
}
/// Write the worker's cancellation marker whole, unless one is already
/// there: the runner reads only that it exists, the flow what it says.
fn mark(work: &Work, marker: &[u8]) -> Result<()> {
    use std::io::Write;
    let path = work.path.with_extension("cancel");
    let mut temp = tempfile::NamedTempFile::new_in(path.parent().ok_or("job path has no parent")?)?;
    temp.write_all(marker)?;
    temp.as_file().sync_all()?;
    match temp.persist_noclobber(&path) {
        Ok(_) => Ok(()),
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e.into()),
    }
}
