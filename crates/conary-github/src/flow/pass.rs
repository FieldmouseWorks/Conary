//! The shared pass entry for CLI passes and event kicks.
use super::jobs::{self, Launch, Purpose};
use super::persist::{self, Result};
use super::*;
use conary_core::op::Repo;
use conary_core::{Policy, Store};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Plugins {
    pub review: Option<PathBuf>,
    pub fix: Option<PathBuf>,
    pub judge: Option<PathBuf>,
    pub proof: Option<PathBuf>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub settings: Settings,
    pub trusted_authors: Vec<String>,
    pub plugins: Plugins,
    pub negative: Option<Negative>,
}

/// Producer policy for the revision's negative evidence. Required before any
/// explicit pass can publish settlement, including delegated bot settlement.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Negative {
    pub check: String,
    pub signer: String,
    pub fresh: Option<String>,
}
impl Negative {
    pub(crate) fn verifier(&self) -> Result<crate::train::Negatives> {
        Ok(crate::train::Negatives {
            check: self.check.clone(),
            signer: self
                .signer
                .parse()
                .map_err(|e: String| -> persist::Error { e.into() })?,
            fresh: self.fresh.clone(),
        })
    }
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub repo: Repo,
    pub pr: u64,
    #[serde(flatten)]
    pub next: Option<Next>,
    pub error: Option<String>,
}

pub struct Pass<'a> {
    pub config: &'a Config,
    pub policy: &'a Policy,
    pub root: &'a Path,
    pub launcher: &'a dyn Launch,
    pub now: u64,
    pub dry_run: bool,
    pub ledger: Option<&'a conary_core::Ledger>,
}
impl Pass<'_> {
    pub fn run<G: Write>(
        &self,
        github: &G,
        repo: &Repo,
        only: Option<u64>,
        mut store: Option<&mut Store>,
    ) -> Result<Vec<Report>> {
        let data = if self.dry_run {
            None
        } else {
            Some(Data::open(self.root, repo)?)
        };
        let mut pulls = github.pulls(&repo.to_string(), crate::api::Pulls::Open)?;
        pulls.sort_by_key(|p| p.number);
        // Read the entire repository even for a targeted kick. Dry runs inspect
        // the same workers without collecting or cancelling them.
        let mut occupancy = Vec::new();
        let mut waiting = Vec::new();
        for pull in &pulls {
            let record = match Data::read(self.root, repo, pull.number) {
                Ok(record) => record,
                Err(_) => {
                    // Unknown ownership holds a slot, but doesn't prevent
                    // independent PRs from being reviewed or repaired.
                    occupancy.push(ProofHolder {
                        number: pull.number,
                        job: Job {
                            id: format!("unreadable-{}", pull.number),
                            revision: Revision::from(pull),
                            state: JobState::Unknown,
                            started: 0,
                            repeated: false,
                        },
                        paths: None,
                    });
                    continue;
                }
            };
            // Queue priority includes PRs excluded by a targeted kick.
            if let Ok(mut facts) = read(
                github,
                repo,
                pull.number,
                self.policy,
                self.config.settings.clone(),
                &self.config.trusted_authors,
            ) {
                let _ = super::observe::hydrate(&mut facts, &record, self.now, true);
                let decision = next(&facts, &record.memory, self.now).0;
                if decision.step == Step::Prove || decision.stage == Stage::ProofSlot {
                    waiting.push(ProofWaiting {
                        number: pull.number,
                    });
                }
            }
            for work in &record.jobs {
                if matches!(work.purpose, Purpose::Proof)
                    && jobs::inspect(work, self.now).map_or(true, |answer| answer.result.is_none())
                {
                    occupancy.push(ProofHolder {
                        number: pull.number,
                        job: work.job.clone(),
                        paths: None,
                    });
                }
            }
        }
        let mut reports = Vec::new();
        for pull in pulls
            .into_iter()
            .filter(|p| only.is_none_or(|n| n == p.number))
        {
            let pr = pull.number;
            let mut report = Report {
                repo: repo.clone(),
                pr,
                next: None,
                error: None,
            };
            let result: Result<()> = (|| {
                let mut record = match &data {
                    Some(data) => data.load(pr)?,
                    None => Data::read(self.root, repo, pr)?,
                };
                let mut facts = read(
                    github,
                    repo,
                    pr,
                    self.policy,
                    self.config.settings.clone(),
                    &self.config.trusted_authors,
                )?;
                super::observe::hydrate(&mut facts, &record, self.now, self.dry_run)?;
                if let Some(ledger) = store.as_deref().map(Store::ledger).or(self.ledger) {
                    super::observe::train(&mut facts, ledger);
                }
                facts.proof_data.live = occupancy.clone();
                facts.proof_data.waiting = waiting.clone();
                facts.failed = record.failed.take();
                facts.failed_review = record.failed_review.take();
                facts.succeeded = std::mem::take(&mut record.succeeded);
                let (mut next, memory) = next(&facts, &record.memory, self.now);
                record.memory = memory;
                if let Some(kind) = plugin_kind(&next.step)
                    && self.path(kind).is_none()
                {
                    next.step = Step::Wait { until: None };
                    next.description = format!("No plugin configured for {kind}");
                }
                if next.step == Step::Prove {
                    waiting.retain(|waiter| waiter.number != pr);
                }
                if self.dry_run && next.step == Step::Prove {
                    occupancy.push(ProofHolder {
                        number: pr,
                        job: Job {
                            id: format!("dry-{pr}"),
                            revision: facts.revision.clone(),
                            state: JobState::Running,
                            started: self.now,
                            repeated: false,
                        },
                        paths: None,
                    });
                }
                report.next = Some(next.clone());
                record.decision = Some(next.clone());
                if let Some(data) = &data {
                    // Save observations before executing. Never act using the previous Memory.
                    data.save(pr, &record)?;
                    let effect = super::execute::act(
                        self,
                        github,
                        &facts,
                        &next,
                        &mut record,
                        data,
                        store.as_deref_mut(),
                    );
                    match effect {
                        Ok(succeeded) => {
                            record.succeeded.extend(succeeded);
                        }
                        Err((kind, error)) => {
                            if let Step::PostReview(role) = &next.step
                                && kind == Failure::Post
                            {
                                record.failed_review = Some(FailedPost {
                                    role: role.clone(),
                                    unavailable: error
                                        .downcast_ref::<crate::ApiError>()
                                        .is_some_and(|e| e.unavailable),
                                });
                            } else {
                                record.failed = Some(kind);
                            }
                            report.error = Some(error.to_string());
                        }
                    }
                    data.save(pr, &record)?;
                    occupancy.retain(|holder| holder.number != pr);
                    for work in &record.jobs {
                        if matches!(work.purpose, Purpose::Proof)
                            && jobs::inspect(work, self.now)
                                .map_or(true, |answer| answer.result.is_none())
                        {
                            occupancy.push(ProofHolder {
                                number: pr,
                                job: work.job.clone(),
                                paths: None,
                            });
                        }
                    }
                }
                Ok(())
            })();
            if let Err(error) = result {
                report.error = Some(error.to_string());
            }
            reports.push(report);
        }
        Ok(reports)
    }
    pub(crate) fn path(&self, kind: &str) -> Option<&Path> {
        match kind {
            "review" => self.config.plugins.review.as_deref(),
            "fix" => self.config.plugins.fix.as_deref(),
            "judge" => self.config.plugins.judge.as_deref(),
            "proof" => self.config.plugins.proof.as_deref(),
            _ => None,
        }
    }
    pub(crate) fn start(
        &self,
        facts: &Facts,
        purpose: Purpose,
        task: conary_core::plugin::Task,
        record: &mut Record,
        data: &Data,
    ) -> Result<()> {
        let kind = match purpose {
            Purpose::Review(_) => "review",
            Purpose::Fix(_) => "fix",
            Purpose::Judge => "judge",
            Purpose::Proof => "proof",
        };
        let executable = self
            .path(kind)
            .ok_or("plugin is not configured")?
            .to_path_buf();
        // Observe the shared issuance high-water mark before allocating. Equal
        // concurrent issuances are reconciled as conflicts, never last-wins.
        let observed = facts
            .judgment_generation
            .into_iter()
            .chain(
                record
                    .memory
                    .judge_issuance
                    .values()
                    .map(|(generation, _)| *generation),
            )
            .max()
            .unwrap_or(0);
        record.sequence = record
            .sequence
            .max(observed)
            .checked_add(1)
            .ok_or("job sequence exhausted")?;
        let path = data.job_path(facts.number, record.sequence);
        let timeout = if kind == "proof" {
            facts.settings.proof_timeout
        } else {
            facts.settings.job_timeout
        };
        let deadline = self.now.saturating_add(timeout);
        persist::save(
            &path,
            &jobs::Invocation {
                executable,
                task,
                deadline,
                response_bytes: 1_048_576,
            },
        )?;
        record.jobs.push(jobs::Work {
            purpose,
            generation: record.sequence,
            job: Job {
                id: format!("{}-{}", facts.number, record.sequence),
                revision: facts.revision.clone(),
                state: JobState::Running,
                started: self.now,
                repeated: false,
            },
            path: path.clone(),
            deadline,
            posted: false,
            recipes: recipes(facts),
        });
        // Persist ownership before spawning. A crash in this window times out;
        // it never starts the same fix twice.
        data.save(facts.number, record)?;
        self.launcher.launch(&path)?;
        Ok(())
    }
}
fn plugin_kind(step: &Step) -> Option<&'static str> {
    match step {
        Step::QueueReview(_) => Some("review"),
        Step::HandFix(_) => Some("fix"),
        Step::Judge => Some("judge"),
        Step::Prove => Some("proof"),
        _ => None,
    }
}
pub(crate) fn recipes(f: &Facts) -> BTreeMap<String, Recipe> {
    f.checks
        .iter()
        .filter(|c| c.state == Ci::Pending)
        .filter_map(|c| {
            f.settings
                .proof_recipe(&c.name)
                .map(|(n, r)| (n.clone(), r.clone()))
        })
        .collect()
}
