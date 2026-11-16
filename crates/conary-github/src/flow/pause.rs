//! A repository's pause: the flow's off switch. It's a file in the
//! repository's flow directory, written and removed by `cn flow pause` and
//! `cn flow resume` with no pass, conaryd, GitHub, build or deploy, and read
//! by every pass before each decision and again before each effect. A paused
//! PR, like a held one, vacates: its live workers are stopped through the
//! flow's stop path ([`super::decide`]), which frees their proof slots, and
//! nothing new starts. What they finished stays, so a resume redoes nothing.
use super::persist::{self, Result};
use conary_core::op::Repo;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// What a pause covers. A narrower scope is another variant, never a second
/// pause.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Scope {
    /// Every PR of the repository.
    Repository,
}

/// A pause in effect, as `cn flow pause` wrote it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pause {
    pub scope: Scope,
    /// Why, in the words of whoever paused it.
    pub reason: String,
    /// When it was written, in Unix seconds.
    pub at: u64,
}
impl Pause {
    /// Whether it stops this PR. Every scope there is covers every PR.
    pub fn covers(&self, _pr: u64) -> bool {
        match self.scope {
            Scope::Repository => true,
        }
    }
}

fn path(root: &Path, repo: &Repo) -> PathBuf {
    persist::dir(root, repo).join("pause")
}

/// The repository's pause, if one is in effect. A pause that can't be read
/// is an error, never no pause.
pub fn read(root: &Path, repo: &Repo) -> Result<Option<Pause>> {
    match std::fs::read(path(root, repo)) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Pause the repository. Always allowed: it takes no lock, so a pass that
/// holds the repository's lock doesn't keep it off, and that pass sees it at
/// its next decision or effect. A second pause replaces the first.
pub fn pause(root: &Path, repo: &Repo, pause: &Pause) -> Result<()> {
    std::fs::create_dir_all(persist::dir(root, repo))?;
    persist::save(&path(root, repo), pause)
}

/// Lift the repository's pause, if it has one. The next pass carries on
/// from each PR's saved state.
pub fn resume(root: &Path, repo: &Repo) -> Result<()> {
    match std::fs::remove_file(path(root, repo)) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    }
    std::fs::File::open(persist::dir(root, repo))?.sync_all()?;
    Ok(())
}

/// An effect a pause, written after its decision, kept from happening. It's
/// neither a failure nor a strike: the next decision reads the pause.
#[derive(Debug)]
pub struct Paused;
impl std::fmt::Display for Paused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the repository was paused before this effect")
    }
}
impl std::error::Error for Paused {}

/// Refuse an effect on the PR while a pause covers it. Read afresh each
/// time, right before the effect.
pub(crate) fn effect(root: &Path, repo: &Repo, pr: u64) -> Result<()> {
    match read(root, repo)? {
        Some(pause) if pause.covers(pr) => Err(Box::new(Paused)),
        _ => Ok(()),
    }
}

/// Whether a step takes work off rather than puts it on: the only steps a
/// pause lets through. Leaving the train and taking GitHub's queued merge
/// off vacate too.
pub(crate) fn vacates(step: &super::Step) -> bool {
    use super::Step;
    matches!(
        step,
        Step::Wait { .. }
            | Step::Retry { .. }
            | Step::StopJob(_)
            | Step::LeaveTrain
            | Step::DisableAutoMerge
    )
}

/// Why a worker was asked to stop, as its cancellation marker records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// Its answer could no longer decide anything.
    Obsolete,
    /// Its repository was paused.
    Paused,
    /// Its PR was held.
    Held,
}
impl Stop {
    pub(crate) fn marker(self) -> &'static [u8] {
        match self {
            Self::Obsolete => b"obsolete\n",
            Self::Paused => b"paused\n",
            Self::Held => b"held\n",
        }
    }
    /// The stop a marker records. Any other marker (a deadline's) is no
    /// stop: its worker failed.
    pub(crate) fn from_marker(marker: &[u8]) -> Option<Self> {
        [Self::Obsolete, Self::Paused, Self::Held]
            .into_iter()
            .find(|stop| stop.marker() == marker)
    }
}
