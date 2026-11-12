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
    pub evidence: Option<Evidence>,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub repo: Repo,
    pub pr: u64,
    #[serde(flatten)]
    pub next: Option<Next>,
    pub error: Option<String>,
    pub owner_approval: Option<String>,
    /// The revision the decision was made on, once the PR was read.
    pub revision: Option<Revision>,
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
        self.config.settings.deferred.validate(
            &self.config.settings,
            self.config.evidence.as_ref().map(|e| e.negative.as_str()),
        )?;
        let data = if self.dry_run {
            None
        } else {
            Some(Data::open(self.root, repo)?)
        };
        let mut recovery = Vec::new();
        if let Some(data) = &data {
            for pr in data.records()? {
                let result: Result<()> = (|| {
                    let mut record = data.load(pr)?;
                    let reconciled = if record.landing.is_some() || !record.pending.is_empty() {
                        match store.as_deref_mut() {
                            Some(store) => super::ledger::reconcile(
                                self,
                                github,
                                repo,
                                data,
                                store,
                                pr,
                                &mut record,
                            ),
                            None => Err("pending landing needs an anchored ledger".into()),
                        }
                    } else {
                        Ok(())
                    };
                    // Selected PRs withdraw their last valid grant even if a
                    // new read fails. Unrelated live permissions are preserved.
                    let selected = only.is_none_or(|n| n == pr);
                    let closed = !selected
                        && !record.live.is_empty()
                        && github
                            .pull(&repo.to_string(), pr)
                            .is_ok_and(|p| p.state != "open");
                    if selected || closed {
                        super::ledger::withdraw_live(pr, data, &mut record)?;
                    }
                    let cleaned = super::ledger::cleanup(github, repo, pr, data, &mut record);
                    reconciled.and(cleaned)
                })();
                if let Err(error) = result {
                    recovery.push((pr, error.to_string()));
                }
            }
        }
        let mut pulls = github.pulls(&repo.to_string(), crate::api::Pulls::Open)?;
        pulls.sort_by_key(|p| {
            (
                !p.labels
                    .iter()
                    .any(|l| l.name == self.config.settings.first_label),
                p.number,
            )
        });
        let mut occupancy = super::occupancy::Occupancy::read(self.root, repo, &pulls);
        occupancy.discover(
            self,
            github,
            repo,
            store.as_deref().map(Store::ledger).or(self.ledger),
        );
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
                owner_approval: None,
                revision: None,
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
                report.revision = Some(facts.revision.clone());
                if let Some((_, error)) = recovery.iter().find(|(number, _)| *number == pr) {
                    // Keep advancing strikes while cleanup blocks effects.
                    record.failed = Some(Failure::Status);
                    report.error = Some(error.clone());
                }
                super::ledger::raised(
                    &mut facts,
                    &mut record.memory,
                    store.as_deref().map(Store::ledger).or(self.ledger),
                );
                if !self.dry_run
                    && let Some(store) = store.as_deref_mut()
                {
                    super::ledger::classify(self, github, &mut facts, &mut record, store)?;
                }
                super::observe::hydrate(&mut facts, &record, self.now, self.dry_run)?;
                facts.dispatches.clone_from(&record.dispatches);
                let prove = |p: &super::deferred::Proof| {
                    self.config
                        .evidence
                        .as_ref()
                        .is_some_and(|e| super::evidence::deferred(github, &facts, e, p).is_ok())
                };
                let seen = super::deferred::observe(github, &facts, self.now, &prove)?;
                if !self.dry_run
                    && seen.apply(&mut record)
                    && let Some(data) = &data
                {
                    data.save(pr, &record)?;
                    facts.dispatches.clone_from(&record.dispatches);
                }
                facts.deferred = seen.deferrals;
                facts.deferred_stale = seen.stale;
                facts.deferred_wake = seen.wake;
                occupancy.apply(&mut facts, self.now)?;
                if let Some(ledger) = store.as_deref().map(Store::ledger).or(self.ledger) {
                    super::observe::train(&mut facts, ledger);
                }
                super::observe::issuances(&mut facts, &record.memory);
                facts.failed = record.failed.take();
                facts.failed_review = record.failed_review.take();
                facts.succeeded = std::mem::take(&mut record.succeeded);
                let approved = facts.owner_approved;
                let (mut next, memory) = next(&facts, &record.memory, self.now);
                record.memory = memory;
                if next.step == Step::Prove {
                    let (ahead, _) = super::proofs::ahead(&facts, None);
                    if ahead > 0 {
                        next.stage = Stage::ProofSlot;
                        next.step = Step::Wait { until: None };
                        next.proof = None;
                        next.description =
                            format!("Waiting for a full plugin proof slot ({ahead} ahead)");
                    }
                }
                if record
                    .landing
                    .iter()
                    .chain(&record.pending)
                    .any(|l| !l.delegated && !l.retry)
                {
                    next.stage = Stage::Merge;
                    next.step = Step::Wait { until: None };
                    next.description =
                        "Waiting to reconcile the submitted merge with the ledger".into();
                }
                if matches!(next.step, Step::Merge | Step::Settle)
                    && facts.classified.class == conary_core::Class::R2
                {
                    next.stage = Stage::Train;
                    next.step = Step::Wait { until: None };
                    next.description =
                        "R2 landing requires the merge train and exact candidate evidence".into();
                }
                if grants(&next, &facts)
                    && let Err(why) = if store.is_none() && (!self.dry_run || self.ledger.is_none())
                    {
                        Err("landing permission requires an anchored ledger".into())
                    } else {
                        super::evidence::authorize(self, github, &facts)
                    }
                {
                    next.step = Step::Wait { until: None };
                    next.stage = Stage::Proof;
                    next.description = why.to_string().chars().take(140).collect();
                }
                if let Step::HandFix(kind) = next.step
                    && let Err(why) = super::handoff::problems(github, &facts, &record.memory, kind)
                {
                    next.step = Step::Wait { until: None };
                    next.description = why;
                }
                if let Some(kind) = plugin_kind(&next.step)
                    && self.path(kind).is_none()
                {
                    next.step = Step::Wait { until: None };
                    next.description = format!("No plugin configured for {kind}");
                }
                if super::approval::required(&facts) && !approved {
                    report.owner_approval =
                        Some(super::approval::owner_approval(repo, pr, &facts.revision));
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
                }
                next.wait_until = next
                    .wait_until
                    .into_iter()
                    .chain(
                        record
                            .jobs
                            .iter()
                            .filter(|w| !w.posted && w.deadline > self.now)
                            .map(|w| w.deadline),
                    )
                    .chain(super::deferred::deadline(
                        &facts,
                        &record.dispatches,
                        self.now,
                    ))
                    .min();
                report.next = Some(next.clone());
                record.decision = Some(next.clone());
                if let Some(data) = &data {
                    data.save(pr, &record)?;
                }
                occupancy.observed(self.root, &facts, &next, self.now, self.dry_run)?;
                Ok(())
            })();
            if let Err(error) = result {
                report.error = Some(error.to_string());
            }
            reports.push(report);
        }
        for (pr, error) in recovery {
            if !reports.iter().any(|r| r.pr == pr) {
                reports.push(Report {
                    repo: repo.clone(),
                    pr,
                    next: None,
                    error: Some(error),
                    owner_approval: None,
                    revision: None,
                });
            }
        }
        reports.sort_by_key(|r| r.pr);
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
        record.sequence = record
            .sequence
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

pub(crate) fn grants(next: &Next, f: &Facts) -> bool {
    matches!(next.step, Step::Merge | Step::Settle | Step::JoinTrain)
        || next.stage == Stage::Train
            && matches!(next.step, Step::Wait { .. })
            && f.train.seat.is_some()
}
