//! Each repo's train in the ledger (`docs/DESIGN.md` section 4.7): the
//! changes waiting in it, its candidates and how each ended, and the
//! candidate it builds next. Only the train's own ops and a change's new
//! head, close or landing move it, so the ledger, replayed, says where every
//! change stands in its train and why.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::Serialize;

use super::{Invalid, Ledger, Status};
use crate::id::{CandidateId, ChangeId, CommitId, RevisionId, TreeId};
use crate::op::{Attest, Body, CheckKind, Left, Member, Outcome, Repo, Verdict};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CandidateEvidence {
    pub kind: CheckKind,
    pub check: String,
    pub result: Outcome,
    pub source: String,
    /// Its producer's attestation, when it had one that held.
    pub attest: Option<Attest>,
    pub at: i64,
}

/// Where a candidate is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum State {
    /// In flight: its gate hasn't said yet.
    Gating,
    /// In flight: green, its changes landing in order.
    Green,
    /// Every change landed through it. Which tree trunk ended on isn't
    /// recorded yet.
    Landed,
    /// Every change landed through it, and trunk ended on `tree`.
    Finished {
        tree: TreeId,
    },
    Red,
    Dropped,
    /// It stopped in flight: `by` left the train, or landed while the gate
    /// was still running. `green` when its gate had said green, so it
    /// stopped landing.
    Broken {
        by: ChangeId,
        green: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Candidate {
    pub id: CandidateId,
    pub repo: Repo,
    /// The branch it was built on, trunk then, and the one it lands onto.
    pub trunk: String,
    pub base: CommitId,
    /// Its changes, in the order they're merged and land.
    pub changes: Vec<Member>,
    pub commit: CommitId,
    pub tree: TreeId,
    /// The red candidate it's a half of.
    pub of: Option<CandidateId>,
    /// Its one change joined alone: it lands only while trunk is `base`.
    pub alone: bool,
    pub at: i64,
    pub evidence: Vec<CandidateEvidence>,
    /// The changes that have landed through it, in order.
    pub landed: Vec<ChangeId>,
    pub state: State,
}

impl Candidate {
    pub fn in_flight(&self) -> bool {
        matches!(self.state, State::Gating | State::Green)
    }

    /// Whether it's green on passes nobody attested: its gate said so before
    /// a required check's pass counted only with its producer's
    /// attestation. A gate says green now only with each required check's
    /// pass attested and kept in the evidence, and a train requires at
    /// least one, so such a green holds none.
    pub fn green_unattested(&self) -> bool {
        self.state == State::Green && self.evidence.iter().all(|e| e.attest.is_none())
    }

    /// Whether trunk ended on the candidate's own tree after its last
    /// landing, so that its evidence is about that landing's tree. The
    /// landings before it made trees no gate ran on.
    pub fn same_tree(&self) -> bool {
        matches!(&self.state, State::Finished { tree } if *tree == self.tree)
    }
}

/// Why a change that left its train can't join it again as it is. Joining
/// again would only build the candidate it already failed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Held {
    /// It was red alone in a candidate: until its PR has a new head.
    Red,
    /// Its head wouldn't merge onto trunk and the changes ahead of it: until
    /// it has a new head, trunk moves, or one of those changes leaves (for
    /// good: coming back doesn't hold it again).
    Conflict,
    /// Its merge onto trunk and the changes ahead of it changed nothing: as
    /// long as a conflict's, since what it does came with one of those
    /// changes or is on trunk already.
    Empty,
    /// GitHub refused its merge: until it has a new head or trunk moves.
    Refused,
}

impl std::fmt::Display for Held {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Red => "it was red alone, until its head moves",
            Self::Conflict => {
                "its head wouldn't merge, until its head or trunk moves or a change it was after leaves"
            }
            Self::Empty => {
                "its merge would change nothing, until its head or trunk moves or a change it was after leaves"
            }
            Self::Refused => "GitHub refused its merge, until its head or trunk moves",
        })
    }
}

/// A change held out of its train, as it left.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct Hold {
    why: Held,
    revision: RevisionId,
    /// How often trunk had moved under the train when it left.
    moves: u64,
    /// The changes it wouldn't merge after, or merged after to no effect.
    ahead: Vec<ChangeId>,
}

/// One repo's train.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct Train {
    /// Every change in the train, waiting or in the candidate in flight, in
    /// the order they joined, each at the revision it joined at.
    joined: Vec<Member>,
    /// The candidate in flight.
    in_flight: Option<CandidateId>,
    /// The halves of red candidates still to go, the next one first, each
    /// with the red candidate it's a half of. A half's changes are still in
    /// `joined`; one that leaves leaves its half too.
    splits: VecDeque<(CandidateId, Vec<ChangeId>)>,
    /// Every candidate the train has built, oldest first.
    candidates: Vec<CandidateId>,
    /// The changes in it that joined alone.
    alone: BTreeSet<ChangeId>,
    /// The changes that left and can't join again as they are.
    held: BTreeMap<ChangeId, Hold>,
    /// How often trunk has moved since the train began: every landing and
    /// every commit that reached it some other way.
    moves: u64,
}

impl Train {
    pub fn joined(&self) -> &[Member] {
        &self.joined
    }

    pub fn in_flight(&self) -> Option<CandidateId> {
        self.in_flight
    }

    pub fn candidates(&self) -> &[CandidateId] {
        &self.candidates
    }

    /// Whether `change` is in the train alone: in a candidate of its own.
    pub fn alone(&self, change: ChangeId) -> bool {
        self.alone.contains(&change)
    }

    /// Why `change` can't join again at `revision`, if it can't.
    pub fn held(&self, change: ChangeId, revision: RevisionId) -> Option<Held> {
        let h = self.held.get(&change)?;
        // A conflict's hold, or an empty merge's, is gone once a change it
        // was after leaves.
        let still = h.revision == revision
            && match h.why {
                Held::Red => true,
                Held::Refused | Held::Conflict | Held::Empty => h.moves == self.moves,
            };
        still.then_some(h.why)
    }

    /// Trunk moved under the train.
    pub(super) fn moved(&mut self) {
        self.moves += 1;
    }

    fn hold(&mut self, change: ChangeId, why: Held, ahead: Vec<ChangeId>) {
        let Some(seat) = self.seat(change) else {
            return;
        };
        let hold = Hold {
            why,
            revision: seat.revision,
            moves: self.moves,
            ahead,
        };
        self.held.insert(change, hold);
    }

    fn seat(&self, change: ChangeId) -> Option<&Member> {
        self.joined.iter().find(|m| m.change == change)
    }

    /// Takes a change out of the train and out of any half it's in, and lets
    /// go of the conflicts and empty merges held for being merged after it.
    fn remove(&mut self, change: ChangeId) {
        self.joined.retain(|m| m.change != change);
        self.alone.remove(&change);
        self.held.retain(|_, h| !h.ahead.contains(&change));
        for (_, half) in &mut self.splits {
            half.retain(|c| *c != change);
        }
    }
}

/// The candidate a train builds next: its changes, in order, and the red
/// candidate it's a half of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Plan {
    pub changes: Vec<Member>,
    pub of: Option<CandidateId>,
}

impl Ledger {
    pub fn train(&self, repo: &Repo) -> Option<&Train> {
        self.trains.get(repo)
    }

    pub fn candidate(&self, id: CandidateId) -> Option<&Candidate> {
        self.candidates.get(&id)
    }

    /// The candidate `repo`'s train builds next, of at most `size` changes,
    /// if it builds one now: none while one is in flight or nothing waits.
    /// A red candidate's halves go first, older half first, each whole and
    /// alone; then the changes waiting, in the order they joined. A change
    /// that joined alone goes by itself, and the changes before it go
    /// without it.
    pub fn next(&self, repo: &Repo, size: usize) -> Option<Plan> {
        let train = self.trains.get(repo)?;
        if train.in_flight.is_some() {
            return None;
        }
        if let Some((of, half)) = train.splits.iter().find(|(_, half)| !half.is_empty()) {
            let changes = half
                .iter()
                .filter_map(|c| train.seat(*c).cloned())
                .collect();
            return Some(Plan {
                changes,
                of: Some(*of),
            });
        }
        let changes: Vec<Member> = match train.joined.first() {
            Some(m) if train.alone(m.change) => vec![m.clone()],
            _ => train
                .joined
                .iter()
                .take_while(|m| !train.alone(m.change))
                .take(size)
                .cloned()
                .collect(),
        };
        (!changes.is_empty()).then_some(Plan { changes, of: None })
    }

    fn train_of(&self, change: ChangeId) -> Option<&Train> {
        self.trains.get(&self.changes.get(&change)?.repo)
    }

    /// The changes `change` would be merged after: the ones that joined
    /// before it. Every candidate holds its changes in that order.
    fn ahead_of(&self, repo: &Repo, change: ChangeId) -> Vec<ChangeId> {
        self.trains.get(repo).map_or_else(Vec::new, |t| {
            t.joined
                .iter()
                .map(|m| m.change)
                .take_while(|c| *c != change)
                .collect()
        })
    }

    pub(super) fn check_join(&self, change: ChangeId, revision: RevisionId) -> Result<(), Invalid> {
        let c = self.existing(change)?;
        match c.status() {
            Status::Landed | Status::Reverted => return Err(Invalid::AlreadyLanded),
            Status::Closed => return Err(Invalid::Closed),
            Status::Open => {}
        }
        if c.revision(revision).is_none() {
            return Err(Invalid::UnknownRevision(revision));
        }
        if c.current != Some(revision) {
            return Err(Invalid::NotCurrent(revision));
        }
        // A change in the train is in it at its head now: a new head takes
        // it out.
        if self.train_of(change).and_then(|t| t.seat(change)).is_some() {
            return Err(Invalid::Duplicate);
        }
        if let Some(why) = self.train_of(change).and_then(|t| t.held(change, revision)) {
            return Err(Invalid::Held(why));
        }
        Ok(())
    }

    pub(super) fn check_leave(&self, change: ChangeId) -> Result<(), Invalid> {
        self.existing(change)?;
        match self.train_of(change) {
            Some(t) if t.seat(change).is_some() => Ok(()),
            _ => Err(Invalid::NotInTrain),
        }
    }

    pub(super) fn check_candidate(
        &self,
        candidate: CandidateId,
        repo: &Repo,
        changes: &[Member],
        of: Option<CandidateId>,
    ) -> Result<(), Invalid> {
        if self.candidates.contains_key(&candidate) {
            return Err(Invalid::Duplicate);
        }
        if self.trains.get(repo).is_some_and(|t| t.in_flight.is_some()) {
            return Err(Invalid::TrainBusy);
        }
        let plan = self.next(repo, usize::MAX).ok_or(Invalid::OutOfTurn)?;
        // A half goes whole; the changes waiting go as any number of the
        // first of them.
        let in_turn = plan.of == of
            && !changes.is_empty()
            && if of.is_some() {
                changes == plan.changes
            } else {
                plan.changes.starts_with(changes)
            };
        if !in_turn {
            return Err(Invalid::OutOfTurn);
        }
        Ok(())
    }

    fn existing_candidate(&self, id: CandidateId) -> Result<&Candidate, Invalid> {
        self.candidates
            .get(&id)
            .ok_or(Invalid::UnknownCandidate(id))
    }

    pub(super) fn check_candidate_evidence(
        &self,
        candidate: CandidateId,
        kind: CheckKind,
        check: &str,
        result: Outcome,
        source: &str,
        attest: Option<&Attest>,
    ) -> Result<(), Invalid> {
        let c = self.existing_candidate(candidate)?;
        if c.evidence.iter().any(|e| {
            (
                e.kind,
                e.check.as_str(),
                e.result,
                e.source.as_str(),
                e.attest.as_ref(),
            ) == (kind, check, result, source, attest)
        }) {
            return Err(Invalid::Duplicate);
        }
        Ok(())
    }

    pub(super) fn check_verdict(
        &self,
        candidate: CandidateId,
        verdict: Verdict,
    ) -> Result<(), Invalid> {
        let c = self.existing_candidate(candidate)?;
        match (&c.state, verdict) {
            (State::Gating, _) => Ok(()),
            // Green alone, but trunk moved before it landed: its gate was
            // for a tree it won't make, so it's built again. Or green on
            // passes nobody attested: what's left of it is built again, and
            // gated as a candidate is now.
            (State::Green, Verdict::Dropped) if c.alone || c.green_unattested() => Ok(()),
            _ => Err(Invalid::NotGating),
        }
    }

    pub(super) fn check_finish(&self, candidate: CandidateId) -> Result<(), Invalid> {
        match self.existing_candidate(candidate)?.state {
            State::Landed => Ok(()),
            _ => Err(Invalid::NotLanded),
        }
    }

    /// Applies one of the train's own ops, which [`Ledger::check`] has
    /// accepted. False for any other op.
    pub(super) fn apply_train(&mut self, body: &Body, at: i64) -> bool {
        match body {
            Body::Join {
                change,
                revision,
                alone,
            } => {
                let repo = self.changes[change].repo.clone();
                let train = self.trains.entry(repo).or_default();
                train.held.remove(change);
                train.joined.push(Member {
                    change: *change,
                    revision: *revision,
                });
                if *alone {
                    train.alone.insert(*change);
                }
            }
            Body::Leave { change, why } => {
                let repo = self.changes[change].repo.clone();
                match why {
                    Left::Unready => {}
                    Left::Conflict | Left::Empty => {
                        let ahead = self.ahead_of(&repo, *change);
                        let train = self.trains.get_mut(&repo).expect("check: it's in it");
                        let why = if *why == Left::Empty {
                            Held::Empty
                        } else {
                            Held::Conflict
                        };
                        train.hold(*change, why, ahead);
                    }
                    Left::Refused => {
                        let train = self.trains.get_mut(&repo).expect("check: it's in it");
                        train.hold(*change, Held::Refused, Vec::new());
                    }
                }
                self.unseat(*change)
            }
            Body::Candidate {
                candidate,
                repo,
                trunk,
                base,
                changes,
                commit,
                tree,
                of,
            } => {
                let train = self.trains.entry(repo.clone()).or_default();
                if of.is_some() {
                    // The half this is: the first that has changes left.
                    while train.splits.front().is_some_and(|(_, h)| h.is_empty()) {
                        train.splits.pop_front();
                    }
                    train.splits.pop_front();
                }
                train.in_flight = Some(*candidate);
                train.candidates.push(*candidate);
                let alone = changes.iter().any(|m| train.alone(m.change));
                self.candidates.insert(
                    *candidate,
                    Candidate {
                        id: *candidate,
                        repo: repo.clone(),
                        trunk: trunk.clone(),
                        base: base.clone(),
                        changes: changes.clone(),
                        commit: commit.clone(),
                        tree: tree.clone(),
                        of: *of,
                        alone,
                        at,
                        evidence: Vec::new(),
                        landed: Vec::new(),
                        state: State::Gating,
                    },
                );
            }
            Body::CandidateEvidence {
                candidate,
                kind,
                check,
                result,
                source,
                attest,
            } => self
                .candidates
                .get_mut(candidate)
                .expect("check refuses evidence on an unknown candidate")
                .evidence
                .push(CandidateEvidence {
                    kind: *kind,
                    check: check.clone(),
                    result: *result,
                    source: source.clone(),
                    attest: attest.clone(),
                    at,
                }),
            Body::Verdict { candidate, verdict } => self.verdict(*candidate, *verdict),
            Body::Finish { candidate, tree } => {
                self.candidates
                    .get_mut(candidate)
                    .expect("check refuses finishing an unknown candidate")
                    .state = State::Finished { tree: tree.clone() };
            }
            _ => return false,
        }
        true
    }

    fn verdict(&mut self, id: CandidateId, verdict: Verdict) {
        let c = self
            .candidates
            .get_mut(&id)
            .expect("check refuses a verdict on an unknown candidate");
        let changes: Vec<ChangeId> = c.changes.iter().map(|m| m.change).collect();
        let of = c.of;
        c.state = match verdict {
            Verdict::Green => State::Green,
            Verdict::Red => State::Red,
            Verdict::Dropped => State::Dropped,
        };
        let train = self
            .trains
            .get_mut(&c.repo)
            .expect("a candidate is in its repo's train");
        match verdict {
            Verdict::Green => return,
            // Red alone, the change leaves; red with others, it splits.
            Verdict::Red if changes.len() == 1 => {
                train.hold(changes[0], Held::Red, Vec::new());
                train.remove(changes[0]);
            }
            Verdict::Red => {
                let (older, newer) = changes.split_at(changes.len().div_ceil(2));
                train.splits.push_front((id, newer.to_vec()));
                train.splits.push_front((id, older.to_vec()));
            }
            Verdict::Dropped => requeue(train, of, &changes),
        }
        train.in_flight = None;
    }

    /// Takes a change out of its repo's train, if it's in it: it left, or
    /// has a new head, or closed. A candidate in flight with it stops there.
    pub(super) fn unseat(&mut self, change: ChangeId) {
        let Some(repo) = self.changes.get(&change).map(|c| c.repo.clone()) else {
            return;
        };
        let Some(train) = self.trains.get_mut(&repo) else {
            return;
        };
        if train.seat(change).is_none() {
            return;
        }
        train.remove(change);
        let Some(id) = train.in_flight else {
            return;
        };
        let c = self
            .candidates
            .get_mut(&id)
            .expect("the candidate in flight is in the ledger");
        if c.changes.iter().any(|m| m.change == change) {
            c.state = State::Broken {
                by: change,
                green: c.state == State::Green,
            };
            let rest: Vec<ChangeId> = c
                .changes
                .iter()
                .map(|m| m.change)
                .filter(|m| !c.landed.contains(m))
                .collect();
            requeue(train, c.of, &rest);
            train.in_flight = None;
        }
    }

    /// The candidate a change's landing came through, if it did: the green
    /// candidate in flight lands its changes in order, and this was the next
    /// of them, at the revision it holds, onto its base if it's alone. With
    /// none in flight, the last candidate, stopped while it was landing with
    /// this change next and still in the train, is the one it came through:
    /// GitHub took the merge before the candidate stopped and showed it only
    /// after, and the train builds nothing until it's shown. Any other
    /// landing takes it out of the train like a new head would.
    pub(super) fn land_through_train(
        &mut self,
        change: ChangeId,
        revision: RevisionId,
        before: &CommitId,
    ) -> Option<CandidateId> {
        let repo = self.changes.get(&change)?.repo.clone();
        let train = self.trains.get_mut(&repo)?;
        train.moved();
        let through = match train.in_flight {
            Some(id) => Some(id).filter(|id| self.candidates[id].state == State::Green),
            None => train.candidates.last().copied().filter(|id| {
                matches!(self.candidates[id].state, State::Broken { green: true, .. })
            }),
        }
        .filter(|id| {
            let c = &self.candidates[id];
            c.changes.get(c.landed.len()) == Some(&Member { change, revision })
                && (!c.alone || *before == c.base)
                && train.seat(change).is_some()
        });
        let Some(id) = through else {
            self.unseat(change);
            return None;
        };
        train.remove(change);
        let c = self
            .candidates
            .get_mut(&id)
            .expect("a train's candidates are in the ledger");
        c.landed.push(change);
        if c.landed.len() == c.changes.len() {
            c.state = State::Landed;
            train.in_flight = None;
        }
        Some(id)
    }
}

/// Puts what's left of a candidate that stopped back in its train: a half
/// at the front of the halves still to go, as it was; anything else among
/// the changes waiting, where they joined.
fn requeue(train: &mut Train, of: Option<CandidateId>, rest: &[ChangeId]) {
    let rest: Vec<ChangeId> = rest
        .iter()
        .copied()
        .filter(|c| train.seat(*c).is_some())
        .collect();
    if let (Some(of), false) = (of, rest.is_empty()) {
        train.splits.push_front((of, rest));
    }
}

#[cfg(test)]
mod tests;
