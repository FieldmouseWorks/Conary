//! Landing a green candidate: each of its changes through its own PR's
//! merge, in order, with nothing merged twice and nothing merged that the
//! candidate no longer stands behind.

use super::*;

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
/// back to it. `Some` when the train waits on GitHub showing the merge.
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
    let (why, description) = if pull.base.name != trunk {
        let why = format!("it targets {}, not {trunk}", pull.base.name);
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

/// How far back trunk is read for a merge GitHub hasn't shown: past any
/// candidate's own landings and the few that land outside it meanwhile.
const UNSEEN: usize = 50;

/// Whether the PR's merge is on trunk already though GitHub doesn't show
/// the PR merged yet: a step took its merge and stopped, or GitHub's answer
/// was lost, before the merge showed. Only a head whose train status says
/// it's landing with this candidate can have been merged by it, and then
/// trunk, read back by first parents to the candidate's base, holds a
/// commit GitHub links to the PR.
fn merge_unseen(
    source: &dyn Source,
    full: &str,
    pr: u64,
    head: &str,
    c: &Candidate,
    at: &str,
) -> Result<bool, TrainError> {
    // Newest first from GitHub: the context's first status is its latest.
    let landing = landing_with(c.id);
    let marked = source
        .statuses(full, head)?
        .into_iter()
        .find(|s| s.context == CONTEXT)
        .is_some_and(|s| {
            s.state == "success" && s.description.as_deref() == Some(landing.as_str())
        });
    if !marked {
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
