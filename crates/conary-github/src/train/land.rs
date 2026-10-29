//! Landing a green candidate: each of its changes through its own PR's
//! merge, in order, with nothing merged twice, nothing merged that the
//! candidate no longer stands behind, and no head's train status a success
//! once its landing is over.

use super::*;
use conary_core::ledger::Status;

/// Lands a green candidate's next change, through its PR's merge at the head
/// the candidate holds. The PR is read first, so a merge made since the last
/// step is recorded rather than tried again, and so is one GitHub took but
/// doesn't show yet ([`merge_unseen`]): the step waits for it. A candidate
/// alone lands only while trunk is still its base: once trunk has moved
/// it's dropped, to be built again on trunk as it is. A PR that targets
/// another branch than trunk now is refused, since its merge would land
/// there. Then the train's status goes on its head, which the repo's rules
/// can require, and GitHub merges it or refuses. A refused PR's status says
/// so before the ledger takes it out, so a step stopped in between comes
/// back to it. The last landing's head is retired first ([`retired`]).
/// `Some` when the train waits on GitHub showing the merge.
pub(super) fn land_next(
    store: &mut Store,
    forge: &dyn Forge,
    source: &dyn Source,
    config: &Config,
    trunk: &str,
    c: &Candidate,
    stepped: &mut Stepped,
) -> Result<Option<Now>, TrainError> {
    let repo = &c.repo;
    retired(store, forge, source, c)?;
    let next = &c.changes[c.landed.len()];
    let pr = pr_of(store, next.change);
    let head = head_of(store, next);
    let moved = |store: &Store| {
        store
            .ledger()
            .candidate(c.id)
            .is_none_or(|now| now.state != State::Green || now.landed.len() > c.landed.len())
    };
    let (_, pull) = ingest::pulled(store, source, config, repo, pr)?;
    if moved(store) {
        return Ok(None);
    }
    let full = repo.to_string();
    let at = forge
        .branch(&full, trunk)?
        .ok_or_else(|| TrainError::NoTrunk(trunk.to_owned()))?;
    if merge_unseen(source, &full, pr, head.as_str(), c, &at)? {
        return Ok(Some(landing(store, c)));
    }
    if c.alone && at != c.base.as_str() {
        dropped(store, forge, c, "was built on a trunk that has moved")?;
        stepped.verdicts.push((c.id, Verdict::Dropped));
        return Ok(None);
    }
    let elsewhere = if c.trunk != trunk {
        Some(format!(
            "candidate {} was built on {}, not {trunk}",
            c.id, c.trunk
        ))
    } else if pull.base.name != trunk {
        Some(format!("it targets {}, not {trunk}", pull.base.name))
    } else {
        None
    };
    let (why, description) = if let Some(why) = elsewhere {
        (why.clone(), why)
    } else {
        forge.mark(
            &full,
            head.as_str(),
            Mark::Landing,
            &landing_with(c.id),
            None,
        )?;
        match forge.land(&full, pr, head.as_str())? {
            PrMerge::Merged => {
                stepped.landed.push(pr);
                ingest::pull(store, source, config, repo, pr)?;
                if moved(store) {
                    return Ok(None);
                }
                return Ok(Some(landing(store, c)));
            }
            PrMerge::Refused(why) => {
                let description = format!("GitHub refused its merge: {why}");
                (why, description)
            }
        }
    };
    // Its head may say it's landing; it isn't, and nothing else may.
    forge.mark(&full, head.as_str(), Mark::Refused, &description, None)?;
    store.record(Body::Leave {
        change: next.change,
        why: Left::Refused,
    })?;
    stepped.refused.push(Refused { pr, why });
    Ok(None)
}

/// The candidate the repo's train is landing: the green one in flight, or,
/// with none in flight, the last one if it stopped while landing and has
/// changes left. A stopped one's next landing may be open still.
fn landing_candidate(store: &Store, repo: &Repo) -> Option<Candidate> {
    let ledger = store.ledger();
    let train = ledger.train(repo)?;
    match train.in_flight() {
        Some(id) => ledger.candidate(id).filter(|c| c.state == State::Green),
        None => train
            .candidates()
            .last()
            .and_then(|id| ledger.candidate(*id))
            .filter(|c| {
                matches!(c.state, State::Broken { green: true, .. })
                    && c.landed.len() < c.changes.len()
            }),
    }
    .cloned()
}

/// The candidate the repo's train is landing ([`landing_candidate`]), and
/// the PR whose landing it has started and no step has settled: that PR's
/// head says it's landing with the candidate. Its merge may be on trunk,
/// unseen, so nothing but a step may take it out meanwhile, nor stop a
/// candidate in flight.
pub(super) fn open_landing(
    store: &Store,
    source: &dyn Source,
    repo: &Repo,
) -> Result<Option<(Candidate, u64)>, TrainError> {
    let Some(c) = landing_candidate(store, repo) else {
        return Ok(None);
    };
    let next = &c.changes[c.landed.len()];
    let (head, pr) = (head_of(store, next), pr_of(store, next.change));
    let open = landing_marked(source, &repo.to_string(), head.as_str(), c.id)?;
    Ok(open.then_some((c, pr)))
}

/// Settles the landing of the train's last candidate if it stopped while
/// landing (a change of it left, closed or has a new head, as ingest saw
/// it), before another candidate is built: its next head may say it's
/// landing, from a step that went on to its merge. A merge GitHub shows is
/// recorded, through that candidate. One GitHub took and doesn't show yet is
/// waited for (`Some`), still the candidate's: nothing is built meanwhile.
/// Otherwise, or once it landed some other way or closed, the head goes to
/// pending, since the candidate isn't landing it any more. Either way its
/// last landing's head is retired ([`retired`]).
pub(super) fn stopped_landing(
    store: &mut Store,
    forge: &dyn Forge,
    source: &dyn Source,
    config: &Config,
    repo: &Repo,
    trunk: &str,
) -> Result<Option<Now>, TrainError> {
    let Some(c) = landing_candidate(store, repo).filter(|c| c.state != State::Green) else {
        return Ok(None);
    };
    let full = repo.to_string();
    let next = c.changes[c.landed.len()].clone();
    let head = head_of(store, &next);
    if landing_marked(source, &full, head.as_str(), c.id)? {
        let pr = pr_of(store, next.change);
        ingest::pull(store, source, config, repo, pr)?;
        let now = store.ledger().change(next.change).expect("in its train");
        let description = match (now.status(), now.landing.as_ref()) {
            // Through it: retired below, as its last landing.
            (_, Some(l)) if l.candidate == Some(c.id) => None,
            (Status::Landed, _) => Some(format!("landed, though not with candidate {}", c.id)),
            (Status::Closed, _) => Some(format!("closed, so candidate {} isn't landing it", c.id)),
            (Status::Open, _) => {
                let at = forge
                    .branch(&full, trunk)?
                    .ok_or_else(|| TrainError::NoTrunk(trunk.to_owned()))?;
                if merge_unseen(source, &full, pr, head.as_str(), &c, &at)? {
                    return Ok(Some(Now::Landing {
                        candidate: c.id,
                        landings: vec![Landing {
                            pr,
                            change: next.change,
                            head,
                        }],
                    }));
                }
                Some(format!("candidate {} stopped, so it isn't landing", c.id))
            }
        };
        if let Some(description) = description {
            forge.mark(&full, head.as_str(), Mark::Waiting, &description, None)?;
        }
    }
    let c = store
        .ledger()
        .candidate(c.id)
        .expect("the train's candidate")
        .clone();
    retired(store, forge, source, &c)?;
    Ok(None)
}

/// What the train's status on a head it's landing says.
fn landing_with(candidate: CandidateId) -> String {
    format!("landing with candidate {candidate}")
}

/// A green candidate waiting on GitHub to show its next landing.
fn landing(store: &Store, c: &Candidate) -> Now {
    Now::Landing {
        candidate: c.id,
        landings: c.changes[c.landed.len()..]
            .iter()
            .map(|m| Landing {
                pr: pr_of(store, m.change),
                change: m.change,
                head: head_of(store, m),
            })
            .collect(),
    }
}

/// Retires a candidate's last landing: its head's train status goes from
/// landing to pending, saying it landed. Only the head being landed has the
/// success, so another PR at that head (one opened again after a revert,
/// say) lands only through the train again. Each landing is retired before
/// the next head is marked, and before a candidate is finished or built
/// after, so only the last can still say it's landing; a step that stopped
/// first retires it on the next.
pub(super) fn retired(
    store: &Store,
    forge: &dyn Forge,
    source: &dyn Source,
    c: &Candidate,
) -> Result<(), TrainError> {
    let Some(last) = c.landed.len().checked_sub(1) else {
        return Ok(());
    };
    let full = c.repo.to_string();
    let head = head_of(store, &c.changes[last]);
    if landing_marked(source, &full, head.as_str(), c.id)? {
        let description = format!("landed with candidate {}", c.id);
        forge.mark(&full, head.as_str(), Mark::Waiting, &description, None)?;
    }
    Ok(())
}

/// Whether a head's train status says it's landing with `candidate`: a
/// step marked it and went on to its merge, which may have happened.
pub(super) fn landing_marked(
    source: &dyn Source,
    full: &str,
    head: &str,
    candidate: CandidateId,
) -> Result<bool, TrainError> {
    // Newest first from GitHub: the context's first status is its latest.
    let landing = landing_with(candidate);
    Ok(source
        .statuses(full, head)?
        .into_iter()
        .find(|s| s.context == CONTEXT)
        .is_some_and(|s| {
            s.state == "success" && s.description.as_deref() == Some(landing.as_str())
        }))
}

/// How far back trunk is read for a merge GitHub hasn't shown: past any
/// candidate's own landings and the few that land outside it meanwhile.
const UNSEEN: usize = 50;

/// Whether the PR's merge is on trunk already though GitHub doesn't show
/// the PR merged yet: a step took its merge and stopped, or GitHub's answer
/// was lost, before the merge showed. Only a head whose train status says
/// it's landing with this candidate can have been merged by it, and then
/// trunk, read back by first parents to the candidate's base, holds a
/// commit GitHub links to the PR.
pub(super) fn merge_unseen(
    source: &dyn Source,
    full: &str,
    pr: u64,
    head: &str,
    c: &Candidate,
    at: &str,
) -> Result<bool, TrainError> {
    if !landing_marked(source, full, head, c.id)? {
        return Ok(false);
    }
    let mut sha = at.to_owned();
    for _ in 0..UNSEEN {
        if sha == c.base.as_str() {
            return Ok(false);
        }
        if source.commit_pulls(full, &sha)?.contains(&pr) {
            return Ok(true);
        }
        match source.commit(full, &sha)?.parents.first() {
            Some(parent) => sha = parent.sha.clone(),
            None => return Ok(false),
        }
    }
    Ok(false)
}
