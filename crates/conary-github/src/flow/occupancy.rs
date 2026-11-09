//! Repository-wide admission state, including PRs omitted by targeted kicks.
use super::jobs::{self, Purpose};
use super::persist::Result;
use super::*;
use std::collections::BTreeMap;

pub(crate) struct Occupancy {
    records: BTreeMap<u64, Record>,
    unknown: Vec<u64>,
    virtual_live: Vec<ProofHolder>,
    queued: std::collections::BTreeSet<u64>,
}
impl Occupancy {
    pub(crate) fn read(
        root: &std::path::Path,
        repo: &conary_core::op::Repo,
        pulls: &[crate::api::PullRequest],
    ) -> Self {
        let mut records = BTreeMap::new();
        let mut unknown = Vec::new();
        for pull in pulls {
            match Data::read(root, repo, pull.number) {
                Ok(record) => {
                    records.insert(pull.number, record);
                }
                Err(_) => unknown.push(pull.number),
            }
        }
        Self {
            records,
            unknown,
            virtual_live: vec![],
            queued: Default::default(),
        }
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
                super::observe::issuances(&mut facts, &memory);
                if let Some(ledger) = ledger {
                    super::observe::train(&mut facts, ledger);
                }
                let decision = next(&facts, &memory, pass.now).0;
                Ok(decision.step == Step::Prove || decision.stage == Stage::ProofSlot)
            })();
            match result {
                Ok(true) => {
                    self.queued.insert(*number);
                }
                Ok(false) => {}
                Err(_) => self.unknown.push(*number),
            }
        }
    }
    pub(crate) fn apply(&self, f: &mut Facts, now: u64) -> Result<()> {
        f.proof_data.live.extend(self.virtual_live.clone());
        for number in &self.unknown {
            f.proof_data.live.push(ProofHolder {
                number: *number,
                paths: None,
                job: Job {
                    id: format!("unreadable-{number}"),
                    revision: Revision::default(),
                    state: JobState::Unknown,
                    started: 0,
                    repeated: false,
                },
            });
        }
        for (number, record) in &self.records {
            let mut holding = self.virtual_live.iter().any(|h| h.number == *number);
            for work in &record.jobs {
                if !matches!(work.purpose, Purpose::Proof) {
                    continue;
                }
                if jobs::inspect(work, now)?.result.is_none() {
                    holding = true;
                    f.proof_data.live.push(ProofHolder {
                        number: *number,
                        job: work.job.clone(),
                        paths: None,
                    });
                }
            }
            if !holding && self.queued.contains(number) {
                f.proof_data.waiting.push(ProofWaiting { number: *number });
            }
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
            if next.step == Step::Prove {
                self.virtual_live.push(ProofHolder {
                    number: f.number,
                    paths: None,
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
