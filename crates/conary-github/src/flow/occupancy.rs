//! Repository-wide admission state, including PRs omitted by targeted kicks.
//! Reading every open PR's record is also where the pass meets an unreadable
//! record or worker answer ([`super::degraded`]), for every PR at once.
use super::degraded::{self, Degradation, Degraded, Reading, Reads};
use super::jobs::{self, Purpose};
use super::persist::Result;
use super::*;
use std::collections::BTreeMap;

pub(crate) struct Occupancy {
    records: BTreeMap<u64, Record>,
    /// PRs whose work can't be told this pass, and why.
    unknown: BTreeMap<u64, &'static str>,
    virtual_live: Vec<ProofHolder>,
    queued: std::collections::BTreeSet<u64>,
    /// Each open PR's unreadable record, or its workers' unreadable answers.
    degraded: BTreeMap<u64, Vec<Degradation>>,
    /// What [`Self::discover`]'s reads of each PR met and answered: what a
    /// PR the pass doesn't visit records ([`Self::record`]). A visited PR's
    /// decision reads it again, and records what that read met.
    reads: BTreeMap<u64, Reads>,
}
impl Occupancy {
    pub(crate) fn read(
        root: &std::path::Path,
        repo: &conary_core::op::Repo,
        pulls: &[crate::api::PullRequest],
        now: u64,
    ) -> Self {
        let mut records = BTreeMap::new();
        let mut unknown = BTreeMap::new();
        let mut found = BTreeMap::new();
        for pull in pulls {
            let mut seen = Vec::new();
            match Data::read(root, repo, pull.number) {
                Ok(record) => {
                    for work in &record.jobs {
                        if let Err(e) = jobs::inspect(work, now) {
                            seen.push(Degradation {
                                what: Degraded::Answer {
                                    job: work.job.id.clone(),
                                    kind: work.purpose.kind(),
                                },
                                why: degraded::why(&*e),
                            });
                        }
                    }
                    records.insert(pull.number, record);
                }
                Err(e) => {
                    unknown.insert(pull.number, "record didn't read");
                    seen.push(Degradation {
                        what: Degraded::Record,
                        why: degraded::why(&*e),
                    });
                }
            }
            if !seen.is_empty() {
                found.insert(pull.number, seen);
            }
        }
        Self {
            records,
            unknown,
            virtual_live: vec![],
            queued: Default::default(),
            degraded: found,
            reads: BTreeMap::new(),
        }
    }
    /// What half-worked reading the PR's record and its workers' answers.
    pub(crate) fn degraded(&self, pr: u64) -> &[Degradation] {
        self.degraded.get(&pr).map_or(&[], Vec::as_slice)
    }
    /// Whether the PR's record read, so its answers were read too.
    pub(crate) fn readable(&self, pr: u64) -> bool {
        self.records.contains_key(&pr)
    }
    /// What half-worked reading `pr`'s record and answers: what this read
    /// met, and what the PR's own decision met reading its answers since.
    pub(crate) fn met(&self, pr: u64, since: &[Degradation]) -> Vec<Degradation> {
        let mut read = self.degraded(pr).to_vec();
        for d in since {
            if !read.iter().any(|r| r.what == d.what) {
                read.push(d.clone());
            }
        }
        read
    }
    /// Whether this pass took the path again for `pr`: its record always,
    /// its answers once the record read. Lookups are the decision's.
    pub(crate) fn again(&self, pr: u64, what: &Degraded) -> bool {
        match what {
            Degraded::Record => true,
            Degraded::Answer { .. } => self.readable(pr),
            Degraded::Lookup { .. } | Degraded::Read { .. } | Degraded::Mark { .. } => false,
        }
    }
    /// Record what this pass met of `pr`'s record and answers, and what
    /// [`Self::discover`]'s reads of it met, and clear what it read again.
    /// For a PR the pass doesn't visit: a visited one's decision reads it
    /// again.
    pub(crate) fn record(
        &self,
        store: &mut conary_core::Store,
        repo: &conary_core::op::Repo,
        pr: u64,
    ) -> Result<()> {
        let reads = self.reads.get(&pr);
        let mut seen = self.degraded(pr).to_vec();
        seen.extend(reads.iter().flat_map(|r| r.met.iter().cloned()));
        degraded::record(store, repo, pr, &seen, |what| {
            self.again(pr, what) || reads.is_some_and(|r| r.answered.contains(what))
        })
    }
    pub(crate) fn discover<G: Write>(
        &mut self,
        pass: &Pass<'_>,
        github: &G,
        repo: &conary_core::op::Repo,
        ledger: Option<&conary_core::Ledger>,
    ) {
        for (number, record) in &self.records {
            let mut reads = Reads::default();
            let result: Result<bool> = (|| {
                let read = read(
                    github,
                    repo,
                    *number,
                    pass.policy,
                    pass.config.settings.clone(),
                    &pass.config.trusted_authors,
                );
                let mut facts = match read {
                    Ok(facts) => {
                        reads.extend(facts.reads.clone());
                        facts
                    }
                    Err(e) => {
                        reads.extend(Reads {
                            met: e.met(),
                            answered: e.reads.answered.clone(),
                        });
                        return Err(e.into());
                    }
                };
                let mut memory = record.memory.clone();
                super::ledger::raised(&mut facts, &mut memory, ledger);
                super::observe::hydrate(&mut facts, record, pass.now, true)?;
                let paused = super::pause::read(pass.root, repo)
                    .inspect_err(|e| reads.met(Reading::Pause, degraded::why(&**e)))?;
                reads.answered(Reading::Pause);
                facts.paused = paused.filter(|p| p.covers(*number));
                super::observe::issuances(&mut facts, &memory);
                if let Some(ledger) = ledger {
                    super::observe::train(&mut facts, ledger);
                }
                let mut decision = next(&facts, &memory, pass.now).0;
                // Kept from work as its own decision will keep it, by a
                // degraded record, answer or mark: no place in the queue.
                degraded::wait(
                    &mut decision,
                    &degraded::failing(&self.met(*number, &facts.unreadable), &facts.reads),
                );
                Ok(super::proofs::slotted(&decision) || decision.stage == Stage::ProofSlot)
            })();
            match result {
                Ok(true) => {
                    self.queued.insert(*number);
                }
                Ok(false) => {}
                Err(_) => {
                    // What failed is recorded for it as a degraded read
                    // when the pass doesn't visit it, and met again by its
                    // own decision when it does.
                    self.unknown
                        .entry(*number)
                        .or_insert("state couldn't be read this pass");
                }
            }
            self.reads.insert(*number, reads);
        }
    }
    pub(crate) fn apply(&self, f: &mut Facts, now: u64) -> Result<()> {
        f.proof_data.live.extend(self.virtual_live.clone());
        let mut uncertain = self.unknown.first_key_value().map(|(n, why)| (*n, *why));
        for (number, record) in &self.records {
            if self.unknown.contains_key(number) {
                continue;
            }
            let mut holding = self.virtual_live.iter().any(|h| h.number == *number);
            let start = f.proof_data.live.len();
            for work in &record.jobs {
                if !matches!(work.purpose, Purpose::Proof) || work.slotless {
                    continue;
                }
                match jobs::inspect(work, now) {
                    Err(_) => {
                        f.proof_data.live.truncate(start);
                        uncertain.get_or_insert((*number, "work didn't read"));
                        holding = true;
                        break;
                    }
                    Ok(completion) if completion.result.is_none() => {
                        holding = true;
                        f.proof_data.live.push(ProofHolder {
                            number: *number,
                            job: work.job.clone(),
                        });
                    }
                    Ok(_) => {}
                }
            }
            if !holding && self.queued.contains(number) {
                f.proof_data.waiting.push(ProofWaiting { number: *number });
            }
        }
        if let Some(by) = uncertain {
            f.proof_data.reserved = f.settings.proof_slots.unwrap_or(0);
            f.proof_data.reserved_by = Some(by);
        }
        Ok(())
    }
    pub(crate) fn observed(
        &mut self,
        root: &std::path::Path,
        f: &Facts,
        next: &Next,
        now: u64,
        dry: bool,
    ) -> Result<()> {
        if dry {
            if super::proofs::slotted(next) {
                self.virtual_live.push(ProofHolder {
                    number: f.number,
                    job: Job {
                        id: format!("dry-{}", f.number),
                        revision: f.revision.clone(),
                        state: JobState::Running,
                        started: now,
                        repeated: false,
                    },
                });
            }
        } else {
            self.records
                .insert(f.number, Data::read(root, &f.repo, f.number)?);
        }
        Ok(())
    }
}
