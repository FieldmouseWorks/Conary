//! The switches in effect, and what each one holds. A switch is something a
//! person or the configuration turned off: the repository's pause, a PR's
//! hold label, an omitted plugin, the train off, no person list, no evidence
//! verifier. Each PR's decision names every switch that stops it, whichever
//! of them decided its step, and [`status`] lists the repository's switches
//! with the PRs and workers they hold, from local state alone.
use super::persist::{self, Result};
use super::*;
use conary_core::op::Repo;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// One switch in effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "switch", rename_all = "snake_case")]
pub enum Switch {
    /// The repository's pause, as `cn flow pause` wrote it.
    Pause { reason: String, at: u64 },
    /// The hold label on the PR.
    Hold { label: String },
    /// No plugin is configured for this kind of work.
    NoPlugin { kind: String },
    /// The merge train is off, so an R2 change can't land.
    TrainOff,
    /// No person list, so no person's approval can count.
    NoPeople,
    /// No evidence verifier, or one that requires no gate, so nothing has
    /// permission to land.
    NoEvidence,
}

/// One switch stopping one PR, and what it holds of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holding {
    #[serde(flatten)]
    pub switch: Switch,
    /// The step the PR would take now with every switch that masks this one
    /// lifted: what this one keeps off.
    pub step: Step,
    /// The PR's workers this switch is stopping.
    pub workers: Vec<String>,
}

/// Every switch that stops the PR. `decided` is the flow's decision before
/// the pass's own checks; a pause or a hold masks every other switch from
/// it, so the step they keep off is decided again with them lifted, its
/// memory thrown away.
pub(crate) fn holding(
    pass: &Pass<'_>,
    f: &Facts,
    memory: &Memory,
    decided: &Next,
    now: u64,
) -> Vec<Holding> {
    let label = &f.settings.hold_label;
    let held = f.labels.contains(label);
    let free;
    let unswitched = if f.paused.is_some() || held {
        let mut lifted = f.clone();
        lifted.paused = None;
        lifted.labels.remove(label);
        // What they're stopping isn't the step they keep off.
        lifted.working.clear();
        let mut memory = memory.clone();
        memory.stopping.retain(|id| !f.working.contains(id));
        free = next(&lifted, &memory, now).0;
        &free
    } else {
        decided
    };
    let step = &unswitched.step;
    let workers: Vec<String> = f.working.iter().cloned().collect();
    let mut switches = Vec::new();
    if let Some(pause) = &f.paused {
        switches.push((
            Switch::Pause {
                reason: pause.reason.clone(),
                at: pause.at,
            },
            workers.clone(),
        ));
    }
    if held {
        switches.push((
            Switch::Hold {
                label: label.clone(),
            },
            workers,
        ));
    }
    if let Some(kind) = super::pass::plugin_kind(step)
        && pass.path(kind).is_none()
    {
        switches.push((Switch::NoPlugin { kind: kind.into() }, vec![]));
    }
    let granting = super::pass::grants(unswitched, f);
    if !f.settings.train
        && matches!(step, Step::Merge | Step::Settle)
        && f.classified.class == conary_core::Class::R2
    {
        switches.push((Switch::TrainOff, vec![]));
    }
    // A person's approval is what the owner label, an R2 change or a
    // judge's owner verdict waits on, at the owner or at a landing step.
    let approval = super::approval::required(f)
        || f.settings
            .owner_label
            .as_ref()
            .is_some_and(|l| f.labels.contains(l));
    let owner_decides = unswitched.stage == Stage::Owner && f.trusted && f.complete && approval;
    if f.settings.people.is_empty() && (owner_decides || granting && super::approval::required(f)) {
        switches.push((Switch::NoPeople, vec![]));
    }
    if granting
        && pass
            .config
            .evidence
            .as_ref()
            .is_none_or(|e| e.require.is_empty())
    {
        switches.push((Switch::NoEvidence, vec![]));
    }
    switches
        .into_iter()
        .map(|(switch, workers)| Holding {
            switch,
            step: step.clone(),
            workers,
        })
        .collect()
}

/// The open PRs the last pass read, which [`status`] reports on: a record
/// stays once its PR closes.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Open {
    pub at: u64,
    pub prs: BTreeSet<u64>,
}
fn open_path(root: &Path, repo: &Repo) -> PathBuf {
    persist::dir(root, repo).join("open")
}
/// Save the open PRs a pass read.
pub(crate) fn opened(root: &Path, repo: &Repo, open: &Open) -> Result<()> {
    persist::save(&open_path(root, repo), open)
}

/// A repository's switches in effect, with what each one holds.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Status {
    pub repo: Repo,
    /// When the last pass read the open PRs, the ones listed here. None
    /// before any pass has.
    pub read_at: Option<u64>,
    pub switches: Vec<InEffect>,
}
/// One switch in effect, and every PR it holds.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct InEffect {
    #[serde(flatten)]
    pub switch: Switch,
    pub prs: Vec<Held>,
}
/// One PR a switch holds.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Held {
    pub pr: u64,
    /// The step it keeps off, as the PR's last decision saw it. None for a
    /// PR no pass has decided yet.
    pub step: Option<Step>,
    /// The PR's workers that haven't answered, read now.
    pub workers: Vec<String>,
}

/// The repository's switches in effect and what each one holds, from local
/// state alone: the pause file, read now, and the configuration in effect,
/// over each open PR's saved decision and workers. Needs no GitHub,
/// conaryd, token or lock, so it answers while they're down or a pass runs.
/// A pause, read here, holds every open PR it covers whether or not a pass
/// has seen it yet. Holds are as the last pass read the labels.
pub fn status(root: &Path, repo: &Repo, config: &Config, now: u64) -> Result<Status> {
    let pause = super::pause::read(root, repo)?;
    let open: Option<Open> = match std::fs::read(open_path(root, repo)) {
        Ok(bytes) => Some(serde_json::from_slice(&bytes)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let mut prs = Vec::new();
    for &pr in open.iter().flat_map(|o| &o.prs) {
        let record = Data::read(root, repo, pr)?;
        let mut workers = Vec::new();
        for work in &record.jobs {
            if !record.memory.retired_jobs.contains(&work.job.id)
                && super::jobs::inspect(work, now)?.result.is_none()
            {
                workers.push(work.job.id.clone());
            }
        }
        prs.push((pr, record.decision, workers));
    }
    let held = |pr: u64, step: Option<Step>, workers: &Vec<String>| Held {
        pr,
        step,
        workers: workers.clone(),
    };
    let mut switches = Vec::new();
    if let Some(pause) = pause {
        let held = prs
            .iter()
            .filter(|(pr, _, _)| pause.covers(*pr))
            .map(|(pr, decision, workers)| {
                // Its last decision's step, or the one its switches keep off.
                let step = decision.as_ref().map(|d| {
                    d.switches
                        .first()
                        .map_or_else(|| d.step.clone(), |h| h.step.clone())
                });
                held(*pr, step, workers)
            })
            .collect();
        switches.push(InEffect {
            switch: Switch::Pause {
                reason: pause.reason,
                at: pause.at,
            },
            prs: held,
        });
    }
    let saved = |switch: &Switch| -> Vec<Held> {
        prs.iter()
            .filter_map(|(pr, decision, workers)| {
                let holding = decision
                    .as_ref()?
                    .switches
                    .iter()
                    .find(|h| h.switch == *switch)?;
                // Only a pause or a hold stops workers; the others keep
                // work from starting.
                let workers = match switch {
                    Switch::Hold { .. } => workers.clone(),
                    _ => Vec::new(),
                };
                Some(held(*pr, Some(holding.step.clone()), &workers))
            })
            .collect()
    };
    let mut holds: Vec<Switch> = Vec::new();
    for (_, decision, _) in &prs {
        for holding in decision.iter().flat_map(|d| &d.switches) {
            if matches!(holding.switch, Switch::Hold { .. }) && !holds.contains(&holding.switch) {
                holds.push(holding.switch.clone());
            }
        }
    }
    let plugins = &config.plugins;
    let configured = [
        ("review", &plugins.review),
        ("fix", &plugins.fix),
        ("judge", &plugins.judge),
        ("proof", &plugins.proof),
    ];
    let settings = &config.settings;
    let off = configured
        .into_iter()
        .filter(|(_, path)| path.is_none())
        .map(|(kind, _)| Switch::NoPlugin { kind: kind.into() })
        .chain((!settings.train).then_some(Switch::TrainOff))
        .chain(settings.people.is_empty().then_some(Switch::NoPeople))
        .chain(
            config
                .evidence
                .as_ref()
                .is_none_or(|e| e.require.is_empty())
                .then_some(Switch::NoEvidence),
        );
    for switch in holds.into_iter().chain(off) {
        switches.push(InEffect {
            prs: saved(&switch),
            switch,
        });
    }
    Ok(Status {
        repo: repo.clone(),
        read_at: open.map(|o| o.at),
        switches,
    })
}
