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
    /// The fix plugin's executable.
    #[schemars(extend("x-spender" = true))]
    pub fix: Option<PathBuf>,
    /// The judge plugin's executable.
    #[schemars(extend("x-spender" = true))]
    pub judge: Option<PathBuf>,
    /// The proof plugin's executable.
    #[schemars(extend("x-spender" = true))]
    pub proof: Option<PathBuf>,
    /// The `job` plugin's executable, which serves the stuck role. Unset, no
    /// review loop or stall is handed to it, and the round limit sends
    /// what's left to a fix and a judge.
    #[schemars(extend("x-spender" = true))]
    pub job: Option<PathBuf>,
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
    /// The door that asks the reviewer for a late review where it takes no
    /// App's ask. Unset, the flow asks with its own comment everywhere.
    #[schemars(extend("x-spender" = false))]
    pub review_ask: Option<super::ask::ReviewAsk>,
}
/// Keys of `execution` are plugin kinds, as the plugin contract names them.
fn kinds(schema: &mut schemars::Schema) {
    let mut kind = schemars::schema_for!(conary_core::plugin::Kind);
    kind.remove("$schema");
    kind.remove("title");
    schema.insert("propertyNames".into(), kind.to_value());
}
impl Config {
    /// The one loader of a flow configuration file, for every reader of it:
    /// its seal keys given as files are read here, at each load.
    pub fn load(path: &Path) -> Result<Self> {
        let mut config: Self = super::schema::load(&std::fs::read(path)?)?.0;
        config.settings.read_key_files()?;
        Ok(config)
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
        self.config.settings.validate_stage_labels()?;
        self.config.settings.validate_red_fix_branch()?;
        self.config.settings.review_adapter.validate()?;
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
        // Records owning deferred dispatches, for those whose PR is closed.
        let mut owning = Vec::new();
        if let Some(data) = &data {
            for pr in data.records()? {
                let result: Result<()> = (|| {
                    let mut record = data.load(pr)?;
                    if !record.dispatches.is_empty() {
                        owning.push(pr);
                    }
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
                    // new read fails. An unrelated live permission is kept
                    // only while its PR reads open: a read that fails
                    // withdraws it, and is a degraded read of the PR.
                    let selected = only.is_none_or(|n| n == pr);
                    let mut unread = None;
                    let closed = !selected
                        && !record.live.is_empty()
                        && match github.pull(&repo.to_string(), pr) {
                            Ok(p) => p.state != "open",
                            Err(e) => {
                                unread = Some(super::Degradation {
                                    what: super::Degraded::Read {
                                        read: super::Reading::Pull,
                                    },
                                    why: super::degraded::why(&e),
                                });
                                true
                            }
                        };
                    if selected || closed {
                        super::ledger::withdraw_live(pr, data, &mut record)?;
                    }
                    let recorded = match (unread, store.as_deref_mut()) {
                        (Some(unread), Some(store)) => {
                            super::degraded::record(store, repo, pr, &[unread], |_| false)
                        }
                        _ => Ok(()),
                    };
                    let cleaned = super::ledger::cleanup(github, repo, pr, data, &mut record);
                    // A cancellation/read error must not preserve a live
                    // permission: revocation and cleanup still run above.
                    cancelled.and(reconciled).and(recorded).and(cleaned)
                })();
                if let Err(error) = result {
                    recovery.push((pr, error.to_string()));
                }
            }
        }
        let mut pulls = github.pulls(&repo.to_string(), crate::api::Pulls::Open)?;
        // No decision reads a PR that isn't open: its deferred runs are
        // stopped here, paused or not, since it can land nothing.
        if let Some(data) = &data {
            for pr in owning
                .into_iter()
                .filter(|pr| !pulls.iter().any(|p| p.number == *pr))
            {
                let result = data.load(pr).and_then(|mut record| {
                    super::deferred::abandon(
                        github,
                        &repo.to_string(),
                        pr,
                        &mut record,
                        data,
                        &self.config.settings.deferred,
                        self.now,
                    )
                });
                if let Err(error) = result {
                    recovery.push((pr, format!("stopping its deferred runs: {error}")));
                }
            }
        }
        // Proof slots, train joins and dry runs' virtual slots go in line.
        pulls.sort_by_key(|p| {
            super::Rank::of(&self.config.settings, &self.config.trusted_authors, p)
        });
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
        // The repository's timed proofs, read once: each decision sizes its
        // proof's deadline from them, and a real pass adds what it timed.
        let mut durations = super::durations::read(self.root, repo).map_err(|e| e.to_string());
        let mut occupancy = super::occupancy::Occupancy::read(self.root, repo, &pulls, self.now);
        occupancy.discover(
            self,
            github,
            repo,
            store.as_deref().map(Store::ledger).or(self.ledger),
            &durations,
        );
        // What half-worked for a PR this pass doesn't visit is recorded
        // now; a visited PR's once its revision is.
        if !self.dry_run
            && let Some(store) = store.as_deref_mut()
        {
            for pull in pulls.iter().filter(|p| only.is_some_and(|n| n != p.number)) {
                if let Err(e) = occupancy.record(store, repo, pull.number) {
                    recovery.push((pull.number, format!("recording what degraded: {e}")));
                }
            }
        }
        let mut reports = Vec::new();
        let stepping = self.config.settings.train && only.is_none();
        let mut granted = Vec::new();
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
                    let recorded = match store.as_deref_mut() {
                        Some(store) if !self.dry_run => occupancy.record(store, repo, pr),
                        _ => Ok(()),
                    };
                    let next = super::degraded::waiting(
                        occupancy.degraded(pr),
                        |w| occupancy.again(pr, w),
                        super::Rank::of(&self.config.settings, &self.config.trusted_authors, &pull)
                            .level,
                        store.as_deref().map(Store::ledger).or(self.ledger),
                        repo,
                        pr,
                    );
                    report.revision = Some(Revision::from(&pull));
                    // An error books the pass again, to read it again.
                    report.error = Some(match recorded {
                        Ok(()) => next.description.clone(),
                        Err(e) => format!("{}; recording it failed: {e}", next.description),
                    });
                    report.next = Some(next);
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
                let mut facts = match read {
                    Ok(facts) => facts,
                    Err(e) => {
                        return self.stopped(
                            store.as_deref_mut(),
                            data.as_ref(),
                            &pull,
                            &mut record,
                            &mut report,
                            (e.met(), &e.reads.answered),
                            &e,
                        );
                    }
                };
                super::approval::labeller(github, &mut facts, &record.memory);
                facts.stuck_role = self.config.plugins.job.is_some();
                report.revision = Some(facts.revision.clone());
                if let Some((_, error)) = recovery.iter().find(|(number, _)| *number == pr) {
                    // Keep advancing strikes while cleanup blocks effects.
                    record.failed = Some(Failure::Status);
                    record.failed_why = Some(error.clone());
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
                let hydrated = super::observe::hydrate(&mut facts, &record, self.now, self.dry_run);
                // What half-worked reading its answers, when occupancy read
                // them and when this decision did: recorded even when the
                // read failed otherwise.
                let read = occupancy.met(pr, &facts.unreadable);
                if !self.dry_run
                    && let Some(store) = store.as_deref_mut()
                    && let Err(e) =
                        super::degraded::record(store, repo, pr, &read, |w| occupancy.again(pr, w))
                {
                    report.error = Some(format!("recording what degraded: {e}"));
                }
                hydrated?;
                facts.dispatches.clone_from(&record.dispatches);
                let seen = super::deferred::hydrate(
                    github,
                    &mut facts,
                    self.config.evidence.as_ref(),
                    self.now,
                )?;
                super::uncertain::hydrate(github, &mut facts, self.config.evidence.as_ref());
                if !self.dry_run
                    && seen.apply(&mut record)
                    && let Some(data) = &data
                {
                    data.save(pr, &record)?;
                    facts.dispatches.clone_from(&record.dispatches);
                }
                if let (Some(data), Ok(timed)) = (&data, &mut durations)
                    && timed.fold(&facts.proof_data.ran)
                {
                    super::durations::save(data, timed)?;
                }
                super::durations::apply(&mut facts, &durations);
                super::proofs::tree(github, &mut facts);
                // Read afresh before each PR's decision: a pause written
                // during the pass stops the PRs after it.
                facts.paused = match super::pause::read(self.root, repo) {
                    Ok(paused) => {
                        facts.reads.answered(super::Reading::Pause);
                        paused.filter(|p| p.covers(pr))
                    }
                    Err(e) => {
                        let found = super::Degradation {
                            what: super::Degraded::Read {
                                read: super::Reading::Pause,
                            },
                            why: super::degraded::why(&*e),
                        };
                        let mut met = vec![found];
                        met.extend(facts.reads.met.iter().cloned());
                        return self.stopped(
                            store.as_deref_mut(),
                            data.as_ref(),
                            &pull,
                            &mut record,
                            &mut report,
                            (met, &facts.reads.answered),
                            &*e,
                        );
                    }
                };
                occupancy.apply(&mut facts, self.now)?;
                if let Some(ledger) = store.as_deref().map(Store::ledger).or(self.ledger) {
                    super::observe::train(&mut facts, ledger);
                }
                super::observe::issuances(&mut facts, &record.memory);
                facts.failed = record.failed.take();
                facts.failed_why = record.failed_why.take();
                facts.succeeded = std::mem::take(&mut record.succeeded);
                let approved = facts.owner_approved;
                facts.judge_reason =
                    super::approval::judge_reason(&record, &facts.revision, self.now);
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
                    .any(|l| !l.retry)
                {
                    next.stage = Stage::Merge;
                    next.step = Step::Wait { until: None };
                    next.description =
                        "Waiting to reconcile the submitted merge with the ledger".into();
                }
                if next.step == Step::Merge && facts.classified.class == conary_core::Class::R2 {
                    next.stage = Stage::Train;
                    next.step = Step::Wait { until: None };
                    next.description =
                        "R2 landing requires the merge train and exact candidate evidence".into();
                }
                // A degraded record, answer or mark counts as failed: the PR
                // waits on it, granted nothing.
                let failed = super::degraded::failing(&read, &facts.reads);
                super::degraded::wait(&mut next, &failed);
                let mut looked = std::collections::BTreeSet::new();
                // The reads it went on past, or that stopped short, have
                // each held what they were for failed already.
                let mut lookups = facts.reads.met.clone();
                if grants(&next, &facts)
                    && let Err(why) = if store.is_none() && (!self.dry_run || self.ledger.is_none())
                    {
                        Err("landing permission requires an anchored ledger".into())
                    } else {
                        super::evidence::authorize(self, github, &facts, &next.step, &mut looked)
                    }
                {
                    if let Some(cant) = why.downcast_ref::<super::evidence::CantTell>()
                        && !lookups.iter().any(|l| l.what == cant.0.what)
                    {
                        lookups.push(cant.0.clone());
                    }
                    next.step = Step::Wait { until: None };
                    next.stage = Stage::Proof;
                    next.description = why.to_string().chars().take(140).collect();
                }
                let again = |what: &super::Degraded| {
                    looked.contains(what) || facts.reads.answered.contains(what)
                };
                if !self.dry_run
                    && let Some(store) = store.as_deref_mut()
                    && let Err(e) = super::degraded::record(store, repo, pr, &lookups, again)
                {
                    report.error = Some(format!("recording what degraded: {e}"));
                }
                super::degraded::name(
                    &mut next,
                    &read,
                    lookups,
                    |w| occupancy.again(pr, w) || again(w),
                    store.as_deref().map(Store::ledger).or(self.ledger),
                    repo,
                    pr,
                );
                // A record, answer or mark that didn't read books the pass
                // again, to read it again.
                if !failed.is_empty() && report.error.is_none() {
                    report.error = Some(next.description.clone());
                }
                if let Step::HandFix(kind) = next.step
                    && let Err(why) = super::handoff::problems(&facts, kind)
                {
                    super::recover::unhanded(&mut next, why);
                }
                if let Step::Judge(_) = next.step
                    && let Err(why) = &facts.author
                {
                    super::recover::unhanded(&mut next, why.clone());
                }
                if let Some(kind) = plugin_kind(&next.step)
                    && self.path(kind).is_none()
                {
                    super::recover::unhanded(&mut next, format!("No plugin configured for {kind}"));
                }
                super::recover::handed(&mut next);
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
                        // What stopped reading before the permission was
                        // used is degraded: recorded, and the decision
                        // saved waits on it, naming it.
                        Err((kind, error)) => {
                            // A stop request has its own persisted attempt
                            // deadline; PR backoff would starve other runs.
                            if !(matches!(next.step, Step::CancelDeferred(_))
                                && kind == Failure::Deferred
                                && error.downcast_ref::<crate::ApiError>().is_some())
                            {
                                record.failed = Some(kind);
                                record.failed_why = Some(error.to_string());
                            }
                            report.error = Some(error.to_string());
                            if let Some(withdrawn) =
                                error.downcast_ref::<super::degraded::Withdrawn>()
                            {
                                if let Some(store) = store.as_deref_mut()
                                    && let Err(e) = super::degraded::record(
                                        store,
                                        repo,
                                        pr,
                                        &withdrawn.0,
                                        |_| false,
                                    )
                                {
                                    report.error =
                                        Some(format!("{error}; recording it failed: {e}"));
                                }
                                super::degraded::wait(&mut next, &withdrawn.0);
                                for d in &withdrawn.0 {
                                    if !next.degraded.iter().any(|n| n.what == d.what) {
                                        next.degraded.push(d.clone());
                                    }
                                }
                            }
                        }
                    }
                    super::recover::answered(&mut next, &record.memory);
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
                if stepping && report.error.is_none() && grants(&next, &facts) {
                    granted.push(super::train::Granted {
                        facts,
                        step: next.step,
                    });
                }
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
        // Once every open PR was seen to, the train steps, still under the
        // repository's lock.
        if stepping
            && let Some(data) = &data
            && let Some(store) = store
        {
            super::train::step(self, github, repo, data, &granted, store, &mut reports)?;
        }
        reports.sort_by_key(|r| r.pr);
        Ok(reports)
    }
    /// The decision for a PR a read it can't go on without failed for: it
    /// waits at no stage, naming the read, and nothing acts for it. What
    /// its reads met (that read first) is recorded, and what they answered
    /// is cleared. Its error books the pass again, to read it again.
    #[allow(clippy::too_many_arguments)]
    fn stopped(
        &self,
        store: Option<&mut Store>,
        data: Option<&Data>,
        pull: &crate::api::PullRequest,
        record: &mut Record,
        report: &mut Report,
        (found, answered): (
            Vec<super::Degradation>,
            &std::collections::BTreeSet<super::Degraded>,
        ),
        error: &dyn std::error::Error,
    ) -> Result<()> {
        let again = |w: &super::Degraded| answered.contains(w);
        let recorded = match store {
            Some(store) if !self.dry_run => {
                super::degraded::record(store, &report.repo, pull.number, &found, again)
                    .map(|()| Some(store.ledger()))
            }
            Some(store) => Ok(Some(store.ledger())),
            None => Ok(self.ledger),
        };
        let next = super::degraded::waiting(
            &found,
            again,
            super::Rank::of(&self.config.settings, &self.config.trusted_authors, pull).level,
            recorded.as_ref().ok().copied().flatten(),
            &report.repo,
            pull.number,
        );
        report.revision = Some(Revision::from(pull));
        report.error = Some(match &recorded {
            Ok(_) => format!("{}; {error}", next.description),
            Err(r) => format!("{}; {error}; recording it failed: {r}", next.description),
        });
        record.decision = Some(next.clone());
        report.next = Some(next);
        if let Some(data) = data {
            data.save(pull.number, record)?;
        }
        Ok(())
    }
    pub(crate) fn path(&self, kind: &str) -> Option<&Path> {
        match kind {
            "fix" => self.config.plugins.fix.as_deref(),
            "judge" => self.config.plugins.judge.as_deref(),
            "proof" => self.config.plugins.proof.as_deref(),
            "job" => self.config.plugins.job.as_deref(),
            _ => None,
        }
    }
    pub(crate) fn start(
        &self,
        facts: &Facts,
        purpose: Purpose,
        task: conary_core::plugin::Task,
        handed: Vec<Option<String>>,
        record: &mut Record,
        data: &Data,
    ) -> Result<()> {
        let kind = match purpose {
            Purpose::Fix(_) => "fix",
            Purpose::Judge => "judge",
            Purpose::Stuck(_) => "job",
            // A proof's preparation is the proof plugin's to answer.
            Purpose::Proof | Purpose::Prepare => "proof",
        };
        // Paused since its decision: nothing is saved or launched.
        super::pause::effect(self.root, &facts.repo, facts.number)?;
        let executable = self
            .path(kind)
            .ok_or("plugin is not configured")?
            .to_path_buf();
        let id = job_id(facts, record)?;
        record.sequence += 1;
        let path = data.job_path(facts.number, record.sequence);
        // A proof's deadline is sized from its own history; its
        // preparation's is `proof_timeout`.
        let sized = matches!(purpose, Purpose::Proof).then(|| super::durations::sized(facts));
        let timeout = match (&purpose, &sized) {
            (_, Some(sized)) => sized.seconds,
            (Purpose::Prepare, _) => facts.settings.proof_timeout,
            _ => facts.settings.job_timeout,
        };
        let deadline = self.now.saturating_add(timeout);
        let tree = match &task {
            conary_core::plugin::Task::Prepare { tree, .. }
            | conary_core::plugin::Task::Proof { tree, .. } => Some(tree.clone()),
            _ => None,
        };
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
            sized,
            job: Job {
                id,
                revision: facts.revision.clone(),
                state: JobState::Running,
                started: self.now,
                repeated: false,
            },
            path: path.clone(),
            deadline,
            posted: false,
            recipes: super::proofs::selected(facts),
            tree,
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
/// The id the next job [`Pass::start`] starts on the PR gets.
pub(super) fn job_id(facts: &Facts, record: &Record) -> Result<String> {
    let sequence = record
        .sequence
        .checked_add(1)
        .ok_or("job sequence exhausted")?;
    Ok(format!("{}-{sequence}", facts.number))
}
pub(super) fn plugin_kind(step: &Step) -> Option<&'static str> {
    match step {
        Step::HandFix(_) => Some("fix"),
        Step::Judge(_) => Some("judge"),
        Step::Prepare | Step::Prove => Some("proof"),
        Step::Stuck(..) => Some("job"),
        _ => None,
    }
}

pub(crate) fn grants(next: &Next, f: &Facts) -> bool {
    matches!(next.step, Step::Merge | Step::JoinTrain)
        || next.stage == Stage::Train
            && matches!(next.step, Step::Wait { .. })
            && f.train.seat.is_some()
}
