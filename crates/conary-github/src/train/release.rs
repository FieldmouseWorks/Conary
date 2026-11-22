//! A change red alone whose gate passed since (`docs/DESIGN.md` section
//! 4.7): a third try someone asked for, or a pass GitHub showed late. The
//! verdict stays, and the change may join again.

use conary_core::op::{Body, Repo};
use conary_core::{CandidateId, Store};
use serde::Serialize;

use super::gate::{Gate, gate};
use super::{Forge, Mark, Settings, TrainError, head_of, pr_of, record};
use crate::api::Source;
use crate::ingest::Config;

/// A PR left held red alone, as GitHub refused what its release takes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Unread {
    pub pr: u64,
    /// What GitHub said.
    pub why: String,
}

/// Releases each change held red alone whose candidate's gate is green now,
/// by the same reading that would have landed it, attestations and all. Its
/// head's failure goes to pending first, ahead of the ledger: while that
/// stands nothing settles it to join. The PRs released, and those GitHub
/// refused the reads or the status of (a commit it no longer has, say):
/// each stays held, and the train goes on without it. GitHub out of reach
/// fails the step, as anywhere else in it.
pub(super) fn release(
    store: &mut Store,
    forge: &dyn Forge,
    source: &dyn Source,
    config: &Config,
    settings: &Settings,
    repo: &Repo,
) -> Result<(Vec<u64>, Vec<Unread>), TrainError> {
    let (mut released, mut unread) = (Vec::new(), Vec::new());
    for id in store.ledger().red_alone(repo) {
        let pr = pr_of(
            store,
            store.ledger().candidate(id).expect("red alone").changes[0].change,
        );
        match release_one(store, forge, source, config, settings, id) {
            Ok(true) => released.push(pr),
            Ok(false) => {}
            Err(TrainError::Api(e)) if !e.unavailable => unread.push(Unread {
                pr,
                why: e.to_string(),
            }),
            Err(e) => return Err(e),
        }
    }
    Ok((released, unread))
}

/// Releases the change `id` holds, if its gate is green now: whether it did.
fn release_one(
    store: &mut Store,
    forge: &dyn Forge,
    source: &dyn Source,
    config: &Config,
    settings: &Settings,
    id: CandidateId,
) -> Result<bool, TrainError> {
    let c = store.ledger().candidate(id).expect("red alone").clone();
    let Gate::Green = gate(store, forge, source, config, settings, &c)? else {
        return Ok(false);
    };
    forge.mark(
        &c.repo.to_string(),
        head_of(store, &c.changes[0]).as_str(),
        Mark::Waiting,
        &format!("candidate {id}'s gate passed since, free to join again"),
        None,
    )?;
    record(store, Body::Release { candidate: id })?;
    Ok(true)
}
