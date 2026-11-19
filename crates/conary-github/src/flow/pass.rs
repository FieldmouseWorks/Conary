//! The shared pass entry for CLI passes and event kicks.
use super::jobs::{self, Launch, Purpose};
use super::persist::{self, Result};
use super::*;
use conary_core::op::Repo;
use conary_core::{Policy, Store};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The executables the flow starts its work with. An omitted one leaves its
/// step waiting.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Plugins {
    /// The review plugin's executable.
    #[schemars(extend("x-spender" = true))]
    pub review: Option<PathBuf>,
    /// The fix plugin's executable.
    #[schemars(extend("x-spender" = true))]
    pub fix: Option<PathBuf>,
    /// The judge plugin's executable.
    #[schemars(extend("x-spender" = true))]
    pub judge: Option<PathBuf>,
    /// The proof plugin's executable.
    #[schemars(extend("x-spender" = true))]
    pub proof: Option<PathBuf>,
}
/// The flow's configuration file, reread at each pass.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// The machine's settings.
    #[schemars(extend("x-spender" = false))]
    pub settings: Settings,
    /// The authors whose PRs the flow works; anyone else's goes to the
    /// owner as an outside contribution.
    #[schemars(extend("x-spender" = false))]
    pub trusted_authors: Vec<String>,
    /// The plugins the flow starts its work with.
    #[schemars(extend("x-spender" = false))]
    pub plugins: Plugins,
    /// The engine, model and effort each plugin kind is asked to run with.
    #[schemars(extend("x-spender" = false), transform = kinds)]
    pub execution: BTreeMap<conary_core::plugin::Kind, conary_core::plugin::Choice>,
    /// The verifier of producers' evidence. Unset, permission to land waits.
    #[schemars(extend("x-spender" = false))]
    pub evidence: Option<Evidence>,
}
/// Keys of `execution` are plugin kinds, as the plugin contract names them.
fn kinds(schema: &mut schemars::Schema) {
    let mut kind = schemars::schema_for!(conary_core::plugin::Kind);
    kind.remove("$schema");
    kind.remove("title");
    schema.insert("propertyNames".into(), kind.to_value());
}
impl Config {
    /// The one loader of a flow configuration file, for every reader of it.
    pub fn load(path: &Path) -> Result<Self> {
        Ok(super::schema::load(&std::fs::read(path)?)?.0)
    }
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
                    // The repository's off switch reaches every owned worker,
                    // even when its PR closed or a later GitHub read fails.
                    let cancelled = super::pause::cancel(self.root, repo, pr, &mut record, false)
                        .and_then(|changed| {
                            if changed {
                                data.save(pr, &record)
                            } else {
                                Ok(())
                            }
                        });
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
                    // A cancellation/read error must not preserve a live
                    // permission: revocation and cleanup still run above.
                    cancelled.and(reconciled).and(cleaned)
                })();
                if let Err(error) = result {
                    recovery.push((pr, error.to_string()));
                }
            }
        }
        let mut pulls = github.pulls(&repo.to_string(), crate::api::Pulls::Open)?;
        // Proof slots, train joins and dry runs' virtual slots go in line.
        pulls.sort_by_key(|p| super::Rank::of(&self.config.settings, p));
        if !self.dry_run {
            super::switches::opened(
                self.root,
                repo,
                &super::switches::Open {
                    at: self.now,
                    prs: pulls.iter().map(|p| p.number).collect(),
                },
            )?;
        }
        let mut occupancy = super::occupancy::Occupancy::read(self.root, repo, &pulls, self.now);
        occupancy.discover(
            self,
            github,
            repo,
            store.as_deref().map(Store::ledger).or(self.ledger),
        );
        // What half-worked for a PR this pass doesn't visit is recorded
        // now; a visited PR's once its revision is.
        if !self.dry_run
            && let Some(store) = store.as_deref_mut()
        {
            for pull in pulls.iter().filter(|p| only.is_some_and(|n| n != p.number)) {
                occupancy.record(store, repo, pull.number)?;
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
                owner_approval: None,
                revision: None,
            };
            let result: Result<()> = (|| {
                if !occupancy.readable(pr) {
                    // Its record didn't read: nothing in it counts, and
                    // nothing is written over it.
                    if !self.dry_run
                        && let Some(store) = store.as_deref_mut()
                    {
                        occupancy.record(store, repo, pr)?;
                    }
                    report.revision = Some(Revision::from(&pull));
                    report.next = Some(super::degraded::waiting(
                        occupancy.degraded(pr),
                        super::Rank::of(&self.config.settings, &pull).level,
                        store.as_deref().map(Store::ledger).or(self.ledger),
                        repo,
                        pr,
                    ));
                    return Ok(());
                }
                let mut record = match &data {
                    Some(data) => data.load(pr)?,
                    None => Data::read(self.root, repo, pr)?,
                };
                let read = read(
                    github,
                    repo,
                    pr,
                    self.policy,
                    self.config.settings.clone(),
                    &self.config.trusted_authors,
                );
                // Cancellation is independent of whichever intention wins
                // below, and precedes hydration's deadline bookkeeping. A
                // held PR is read in full, so a hold whose read fails still
                // stops its work, by the label the open list showed.
                let hold = &self.config.settings.hold_label;
                let held = match &read {
                    Ok(facts) => facts.labels.contains(hold),
                    Err(_) => pull.labels.iter().any(|l| l.name == *hold),
                };
                if let Some(data) = &data
                    && super::pause::cancel(self.root, repo, pr, &mut record, held)?
                {
                    data.save(pr, &record)?;
                }
                let mut facts = read?;
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
                    occupancy.record(store, repo, pr)?;
                }
                super::observe::hydrate(&mut facts, &record, self.now, self.dry_run)?;
                // Read afresh before each PR's decision: a pause written
                // during the pass stops the PRs after it.
                facts.paused = super::pause::read(self.root, repo)?.filter(|p| p.covers(pr));
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
                next.switches =
                    super::switches::holding(self, &facts, &record.memory, &next, self.now);
                record.memory = memory;
                if super::proofs::slotted(&next) {
                    let ahead = super::proofs::ahead(&facts);
                    if ahead > 0 {
                        next.stage = Stage::ProofSlot;
                        next.step = Step::Wait { until: None };
                        next.proof = None;
                        next.description = format!(
                            "Waiting for a full plugin proof slot ({ahead} ahead{})",
                            super::proofs::held_by(&facts)
                        );
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
                let mut looked = std::collections::BTreeSet::new();
                let mut lookups = Vec::new();
                if grants(&next, &facts)
                    && let Err(why) = if store.is_none() && (!self.dry_run || self.ledger.is_none())
                    {
                        Err("landing permission requires an anchored ledger".into())
                    } else {
                        super::evidence::authorize(self, github, &facts, &mut looked)
                    }
                {
                    if let Some(cant) = why.downcast_ref::<super::evidence::CantTell>() {
                        lookups.push(cant.0.clone());
                    }
                    next.step = Step::Wait { until: None };
                    next.stage = Stage::Proof;
                    next.description = why.to_string().chars().take(140).collect();
                }
                if !self.dry_run
                    && let Some(store) = store.as_deref_mut()
                {
                    super::degraded::record(
                        store,
                        repo,
                        pr,
                        &lookups,
                        |what| matches!(what, super::Degraded::Lookup { check } if looked.contains(check)),
                    )?;
                }
                super::degraded::name(
                    &mut next,
                    occupancy.degraded(pr),
                    lookups,
                    &looked,
                    store.as_deref().map(Store::ledger).or(self.ledger),
                    repo,
                    pr,
                );
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
                        // A pause the effect read is no failure: the next
                        // decision reads it.
                        Err((_, error)) if error.is::<super::pause::Paused>() => {
                            report.error = Some(error.to_string());
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
        handed: Vec<Option<Handed>>,
        record: &mut Record,
        data: &Data,
    ) -> Result<()> {
        let kind = match purpose {
            Purpose::Review(_) => "review",
            Purpose::Fix(_) => "fix",
            Purpose::Judge => "judge",
            // A proof's preparation is the proof plugin's to answer.
            Purpose::Proof | Purpose::Prepare => "proof",
        };
        // Paused since its decision: nothing is saved or launched.
        super::pause::effect(self.root, &facts.repo, facts.number)?;
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
        let choice = task
            .kind()
            .and_then(|kind| self.config.execution.get(&kind))
            .cloned();
        persist::save(
            &path,
            &jobs::Invocation {
                executable,
                task,
                choice,
                deadline,
                response_bytes: 1_048_576,
            },
        )?;
        // Only a proof its preparation said needs no slot holds none.
        let slotless = matches!(purpose, Purpose::Proof) && !super::proofs::needs_slot(facts);
        record.jobs.push(jobs::Work {
            purpose,
            slotless,
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
            recipes: super::proofs::selected(facts),
            handed,
            sent: Default::default(),
        });
        // Persist ownership before spawning. A crash in this window times out;
        // it never starts the same fix twice.
        data.save(facts.number, record)?;
        self.launcher.launch(&path)?;
        Ok(())
    }
}
pub(super) fn plugin_kind(step: &Step) -> Option<&'static str> {
    match step {
        Step::QueueReview(_) => Some("review"),
        Step::HandFix(_) => Some("fix"),
        Step::Judge(_) => Some("judge"),
        Step::Prepare | Step::Prove => Some("proof"),
        _ => None,
    }
}

pub(crate) fn grants(next: &Next, f: &Facts) -> bool {
    matches!(next.step, Step::Merge | Step::Settle | Step::JoinTrain)
        || next.stage == Stage::Train
            && matches!(next.step, Step::Wait { .. })
            && f.train.seat.is_some()
}
