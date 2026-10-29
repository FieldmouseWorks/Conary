//! Each repo's train in the ledger (`docs/DESIGN.md` section 4.7): the
//! changes waiting in it, its candidates and how each ended, and the
//! candidate it builds next. Only the train's own ops and a change's new
//! head, close or landing move it, so the ledger, replayed, says where every
//! change stands in its train and why.

use std::collections::VecDeque;

use serde::Serialize;

use super::{Invalid, Ledger, Status};
use crate::id::{CandidateId, ChangeId, CommitId, RevisionId, TreeId};
use crate::op::{Body, CheckKind, Member, Outcome, Repo, Verdict};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CandidateEvidence {
    pub kind: CheckKind,
    pub check: String,
    pub result: Outcome,
    pub source: String,
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
    /// was still running.
    Broken {
        by: ChangeId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Candidate {
    pub id: CandidateId,
    pub repo: Repo,
    pub base: CommitId,
    /// Its changes, in the order they're merged and land.
    pub changes: Vec<Member>,
    pub commit: CommitId,
    pub tree: TreeId,
    /// The red candidate it's a half of.
    pub of: Option<CandidateId>,
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

    /// Whether trunk ended on the candidate's own tree after its last
    /// landing, so that its evidence is about that landing's tree. The
    /// landings before it made trees no gate ran on.
    pub fn same_tree(&self) -> bool {
        matches!(&self.state, State::Finished { tree } if *tree == self.tree)
    }
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

    fn seat(&self, change: ChangeId) -> Option<&Member> {
        self.joined.iter().find(|m| m.change == change)
    }

    /// Takes a change out of the train and out of any half it's in.
    fn remove(&mut self, change: ChangeId) {
        self.joined.retain(|m| m.change != change);
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
    /// alone; then the changes waiting, in the order they joined.
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
        let changes: Vec<Member> = train.joined.iter().take(size).cloned().collect();
        (!changes.is_empty()).then_some(Plan { changes, of: None })
    }

    fn train_of(&self, change: ChangeId) -> Option<&Train> {
        self.trains.get(&self.changes.get(&change)?.repo)
    }

    pub(super) fn check_join(&self, change: ChangeId, revision: RevisionId) -> Result<(), Invalid> {
        let c = self.existing(change)?;
        match c.status() {
            Status::Landed => return Err(Invalid::AlreadyLanded),
            Status::Closed => return Err(Invalid::Closed),
            Status::Open => {}
        }
        if c.revision(revision).is_none() {
            return Err(Invalid::UnknownRevision(revision));
        }
        // Which revision is the PR's head now is GitHub's to say, not the
        // ledger's: a head pushed back to an older one is a revision it
        // already holds.
        if self
            .train_of(change)
            .and_then(|t| t.seat(change))
            .is_some_and(|m| m.revision == revision)
        {
            return Err(Invalid::Duplicate);
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
    ) -> Result<(), Invalid> {
        let c = self.existing_candidate(candidate)?;
        if c.evidence.iter().any(|e| {
            (e.kind, e.check.as_str(), e.result, e.source.as_str()) == (kind, check, result, source)
        }) {
            return Err(Invalid::Duplicate);
        }
        Ok(())
    }

    pub(super) fn check_verdict(&self, candidate: CandidateId) -> Result<(), Invalid> {
        match self.existing_candidate(candidate)?.state {
            State::Gating => Ok(()),
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
            Body::Join { change, revision } => {
                self.unseat(*change);
                let repo = self.changes[change].repo.clone();
                self.trains.entry(repo).or_default().joined.push(Member {
                    change: *change,
                    revision: *revision,
                });
            }
            Body::Leave { change, .. } => self.unseat(*change),
            Body::Candidate {
                candidate,
                repo,
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
                self.candidates.insert(
                    *candidate,
                    Candidate {
                        id: *candidate,
                        repo: repo.clone(),
                        base: base.clone(),
                        changes: changes.clone(),
                        commit: commit.clone(),
                        tree: tree.clone(),
                        of: *of,
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
            Verdict::Red if changes.len() == 1 => train.remove(changes[0]),
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
            c.state = State::Broken { by: change };
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

    /// The candidate a change's landing came through, if it did: it was in
    /// the green candidate in flight, at the revision that landed. Any other
    /// landing takes it out of the train like a new head would.
    pub(super) fn land_through_train(
        &mut self,
        change: ChangeId,
        revision: RevisionId,
    ) -> Option<CandidateId> {
        let repo = self.changes.get(&change)?.repo.clone();
        let train = self.trains.get_mut(&repo)?;
        let through = train.in_flight.filter(|id| {
            let c = &self.candidates[id];
            c.state == State::Green
                && c.changes
                    .iter()
                    .any(|m| (m.change, m.revision) == (change, revision))
        });
        let Some(id) = through else {
            self.unseat(change);
            return None;
        };
        train.remove(change);
        let c = self
            .candidates
            .get_mut(&id)
            .expect("the candidate in flight is in the ledger");
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
