//! Every operation of a train holds the ledger's lock throughout, wherever
//! it's called, a flow pass included: what it reads of the ledger, the
//! train's statuses and merges it makes on GitHub and the ops it records
//! go together, so no other writer's step, join or departure lands between
//! them (`docs/DESIGN.md`, R2). A store otherwise takes the lock only for
//! each write.

use super::*;

/// Puts a PR in its repo's train at `head`, the head everything but the
/// gate said can land, once the ledger holds the PR as GitHub shows it now
/// and that is its head. Its class is the policy's ([`class::of`], which
/// records it): an R2 change rides alone, in a candidate of its own, which
/// lands only while trunk is still its base. With `negatives`, only once
/// its revision's negative counts ([`Negatives`]).
/// `settled` is the raise labels the steward settled it with, as `cn class`
/// told it: it joins only when they're every raise its change has once
/// it's classed with its labels now, since a raise the steward didn't
/// settle it with, recorded by anyone, would land on a settlement made
/// without it ([`Joined::Raised`]).
/// Its head's train status goes to pending first, so a success left from an
/// earlier time in the train can't let anything merge it.
#[allow(clippy::too_many_arguments)]
pub fn join(
    store: &mut Store,
    forge: &dyn Forge,
    source: &dyn Source,
    config: &Config,
    repo: &Repo,
    pr: u64,
    head: &str,
    settled: &[String],
    policy: &Policy,
    negatives: Option<&Negatives>,
) -> Result<Joined, TrainError> {
    store.holding(|store| {
        held_join(
            store, forge, source, config, repo, pr, head, settled, policy, negatives,
        )
    })?
}

/// Takes a PR out of its repo's train, its head's train status pending
/// first. Not while a landing is open, its head saying it's landing with
/// the candidate it's next in, its merge maybe on trunk, not yet shown: then every
/// change the candidate in flight has left to land stays, since leaving
/// would stop the candidate, and of one that stopped while landing, the
/// change it was landing. Only a step settles that.
pub fn leave(
    store: &mut Store,
    forge: &dyn Forge,
    source: &dyn Source,
    repo: &Repo,
    pr: u64,
    why: Left,
) -> Result<Leaving, TrainError> {
    store.holding(|store| held_leave(store, forge, source, repo, pr, why))?
}

/// Drops a candidate whose gate won't finish, the stall watchdog's last
/// word: its changes wait again as they joined. Green and red are the gate's
/// word, which [`step`] reads, never a hand's. Not while a landing of it is
/// open, its next head saying it's landing: that's a step's to settle.
pub fn drop_candidate(
    store: &mut Store,
    forge: &dyn Forge,
    source: &dyn Source,
    repo: &Repo,
    candidate: CandidateId,
) -> Result<(), TrainError> {
    store.holding(|store| held_drop_candidate(store, forge, source, repo, candidate))?
}

/// One step of a repo's train, the whole of what it does on an event. It
/// finishes the candidates whose landings all came through, builds the next
/// candidate when none is in flight, reads the gate of the one gating and
/// says its verdict once its checks have, lands a green one's changes in
/// order, and deletes the branches of every candidate that isn't in flight.
/// It goes on until the train waits on something: a gate still running, a
/// merge GitHub hasn't shown yet, or nothing to do. It holds the ledger's
/// lock throughout, so two steps never build or land at once. Trunk is the
/// repo's default branch as GitHub says it is when the step starts: the
/// branch join, landing and ingest all mean, never a setting of its own. A
/// step with no check required is refused before it does anything.
pub fn step(
    store: &mut Store,
    forge: &dyn Forge,
    source: &dyn Source,
    config: &Config,
    repo: &Repo,
    settings: &Settings,
) -> Result<Stepped, TrainError> {
    if settings.require.is_empty() {
        return Err(TrainError::NothingRequired);
    }
    store.holding(|store| held_step(store, forge, source, config, repo, settings))?
}
