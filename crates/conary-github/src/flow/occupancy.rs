//! Repository-wide admission state, including PRs omitted by targeted kicks.
//! Reading every open PR's record is also where the pass meets an unreadable
//! record or worker answer ([`super::degraded`]), for every PR at once.
use super::degraded::{self, Degradation, Degraded};
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
    /// Record what this pass met of `pr`'s record and answers, and clear
    /// what it read again: the record always, the answers once it read.
    pub(crate) fn record(
        &self,
        store: &mut conary_core::Store,
        repo: &conary_core::op::Repo,
        pr: u64,
    ) -> Result<()> {
        let readable = self.readable(pr);
        degraded::record(store, repo, pr, self.degraded(pr), |what| match what {
            Degraded::Record => true,
            Degraded::Answer { .. } => readable,
            Degraded::Lookup { .. } => false,
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
            let result: Result<bool> = (|| {
                let mut facts = read(
                    github,
                    repo,
                    *number,
                    pass.policy,
                    pass.config.settings.clone(),
                    &pass.config.trusted_authors,
                )?;
                let mut memory = record.memory.clone();
                super::ledger::raised(&mut facts, &mut memory, ledger);
                super::observe::hydrate(&mut facts, record, pass.now, true)?;
                facts.paused = super::pause::read(pass.root, repo)?.filter(|p| p.covers(*number));
                super::observe::issuances(&mut facts, &memory);
                if let Some(ledger) = ledger {
                    super::observe::train(&mut facts, ledger);
                }
                let decision = next(&facts, &memory, pass.now).0;
                Ok(super::proofs::slotted(&decision) || decision.stage == Stage::ProofSlot)
            })();
            match result {
                Ok(true) => {
                    self.queued.insert(*number);
                }
                Ok(false) => {}
                Err(_) => {
                    // Not a degraded path of its own: what failed is
                    // GitHub's read or the pause's, which the PR's own
                    // decision meets.
                    self.unknown
                        .entry(*number)
                        .or_insert("state couldn't be read this pass");
                }
            }
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
