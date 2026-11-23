//! Project validated worker results into the decision engine's service facts.
use super::jobs::{self, Completion, Purpose};
use super::persist::Result;
use super::*;
use conary_core::plugin;

pub(crate) fn hydrate(f: &mut Facts, record: &Record, now: u64, dry_run: bool) -> Result<()> {
    hydrate_with(f, record, now, dry_run, None)
}

/// Cleanup may race the expected exit of work it already cancelled. For
/// comparison only, keep those workers in their observed pending state.
/// Never use this projection to decide work or to validate a landing grant.
pub(crate) fn hydrate_cleanup(
    f: &mut Facts,
    record: &Record,
    now: u64,
    working: &std::collections::BTreeSet<String>,
) -> Result<()> {
    hydrate_with(f, record, now, true, Some(working))
}
fn hydrate_with(
    f: &mut Facts,
    record: &Record,
    now: u64,
    dry_run: bool,
    working: Option<&std::collections::BTreeSet<String>>,
) -> Result<()> {
    for work in &record.jobs {
        // An answer or stop marker that doesn't read is taken as still
        // running, as occupancy takes it, and kept in the facts: the PR's
        // decision waits on it, nothing is granted while it lasts, and one
        // that stops reading before permission is used withdraws it
        // ([`super::degraded`]). Any other failure is the pass's.
        let read: Result<_> = jobs::stopped(work)
            .map_err(super::persist::Error::from)
            .and_then(|stop| {
                Ok(
                    if working.is_some_and(|ids| ids.contains(&work.job.id)) && stop.is_some() {
                        (stop, Completion::default())
                    } else if dry_run {
                        (stop, jobs::inspect(work, now)?)
                    } else {
                        (stop, jobs::completion(work, now)?)
                    },
                )
            });
        let (stop, completion) = match read {
            Err(e) if e.is::<jobs::Unreadable>() => {
                f.unreadable.push(super::Degradation {
                    what: super::Degraded::Answer {
                        job: work.job.id.clone(),
                        kind: work.purpose.kind(),
                    },
                    why: super::degraded::why(&*e),
                });
                (None, Completion::default())
            }
            read => read?,
        };
        // A fix's validated answer names the session that pushed the head,
        // which no trailer can.
        if let Some(Ok(response)) = &completion.result
            && let plugin::Answer::Fix {
                result: plugin::FixResult::Pushed { head },
                ..
            } = &response.answer
            && head == &f.revision.head
        {
            f.author = Ok(response.by.clone());
        }
        let mut job = work.job.clone();
        // Stopped work, obsolete, paused or held, is neither an answer nor
        // a failure: it's retired, and its request starts again if it's
        // still wanted.
        if stop.is_some() && completion.result.is_some() {
            f.stopped_jobs.insert(job.id.clone());
            continue;
        }
        if completion.result.is_none() {
            f.working.insert(job.id.clone());
        }
        job.state = match &completion.result {
            None => JobState::Running,
            Some(Ok(_)) => JobState::Done,
            Some(Err(_)) => JobState::Broken,
        };
        // Each check a proof or its preparation was asked to run reached
        // its deadline on the tree it was asked for: the count
        // `proofs::stalled` stops at. Stopped work never gets here.
        if matches!(work.purpose, Purpose::Proof | Purpose::Prepare)
            && let Some(tree) = &work.tree
            && work.reached_deadline(&completion)
        {
            f.proof_data
                .deadlines
                .extend(work.recipes.iter().map(|(check, recipe)| Reached {
                    check: check.clone(),
                    recipe: recipe.clone(),
                    tree: tree.clone(),
                    at: work.deadline,
                }));
        }
        match &work.purpose {
            Purpose::Fix(kind) => {
                let answer = match &completion.result {
                    Some(Ok(plugin::Response {
                        answer: plugin::Answer::Fix { result, answers },
                        ..
                    })) => match result {
                        plugin::FixResult::Pushed { head } => {
                            fixed(work, Some(head.clone()), answers.as_deref())
                        }
                        plugin::FixResult::Unchanged => fixed(work, None, answers.as_deref()),
                        plugin::FixResult::Unable { .. } => FixResult::Missing,
                    },
                    Some(Ok(_)) => FixResult::Missing,
                    Some(Err(_)) => FixResult::Invalid,
                    None => FixResult::Missing,
                };
                // A fix whose answers went up and were read back is
                // answered, as a judge's is: never a job that ended without
                // its answer, however many rounds follow on its revision.
                if work.posted && job.state == JobState::Done {
                    job.state = JobState::Answered;
                }
                f.jobs.insert(*kind, job.clone());
                f.claims.push(FixClaim {
                    kind: *kind,
                    job,
                    answer,
                    posted: work.posted,
                });
            }
            Purpose::Judge => {
                // Plugin responses are proposals, not authenticated issuances.
                // The producer publishes its sealed, independently attested
                // judgment separately. Until a generation is read the job is
                // owed its answer; `state::next` then reads whether it came,
                // against every generation it knows, by what this job asked.
                if job.state == JobState::Done && job.revision == f.revision {
                    f.judge_requests
                        .insert(job.id.clone(), super::judging::asked(work)?);
                    if f.judgment_generation.is_none() {
                        job.state = JobState::Unknown;
                    }
                }
                f.jobs.insert(Kind::Judge, job);
            }
            Purpose::Stuck(_) => {
                if let Some(Ok(plugin::Response {
                    answer: plugin::Answer::Job { summary, next },
                    by,
                    ..
                })) = &completion.result
                {
                    // The stuck role's job is one turn that ends `done`: one
                    // that asks or awaits settled nothing, and its loop isn't
                    // broken on it ([`super::stuck::ENDED`]).
                    if *next != plugin::Next::Done {
                        job.state = JobState::Broken;
                    }
                    // Its pushed head's author is its session, as a fix's is.
                    if super::stuck::pushed(summary) == Some(f.revision.head.as_str()) {
                        f.author = Ok(by.clone());
                    }
                    if f.followup.is_none() {
                        f.followup = super::stuck::followup(summary, &f.repo);
                    }
                }
                f.jobs.insert(Kind::Stuck, job);
            }
            Purpose::Proof => {
                if job.revision == f.revision
                    && work.recipes == super::proofs::selected(f)
                    && matches!(completion.result, Some(Ok(_)))
                {
                    // Plugin identity is an assertion, not an evidence attestation.
                    // The producer must publish its own attested check on GitHub.
                    f.proof = Proof::Running;
                } else {
                    f.proof_data.records.push(ProofRecord {
                        job,
                        recipes: work.recipes.clone(),
                        verdicts: None,
                        note_needed: false,
                    });
                }
            }
            // Readiness only: nothing a preparation answers is a verdict,
            // and a failure is a preparation that failed. An answer to
            // another kind of work never gets here: it doesn't read.
            Purpose::Prepare => f.proof_data.prepared.push(Preparation {
                answer: match &completion.result {
                    Some(Ok(plugin::Response {
                        answer: plugin::Answer::Prepare { ready, slot, why },
                        ..
                    })) => Some(Readiness {
                        ready: *ready,
                        slot: *slot,
                        why: why.clone(),
                    }),
                    _ => None,
                },
                ended: completion.result.is_some().then_some(completion.at),
                job,
                recipes: work.recipes.clone(),
            }),
        }
    }
    Ok(())
}
/// A fix's answer, each of its answers on the thread it was handed. An
/// answer to anything it wasn't handed is invalid.
fn fixed(
    work: &jobs::Work,
    pushed: Option<String>,
    answers: Option<&[plugin::FindingAnswer]>,
) -> FixResult {
    let mut threads = Vec::new();
    for answer in answers.unwrap_or_default() {
        match work.handed.get(answer.problem) {
            Some(Some(thread)) => threads.push((thread.clone(), answer.fixed)),
            _ => return FixResult::Invalid,
        }
    }
    FixResult::Answer(FixAnswer {
        pushed,
        answers: threads,
        description: None,
        comment: false,
    })
}

pub(crate) fn train(f: &mut Facts, ledger: &conary_core::Ledger) {
    let Some(change) = ledger.by_pr(&f.repo, f.number) else {
        return;
    };
    let Some(train) = ledger.train(&f.repo) else {
        return;
    };
    if let Some(revision) = change.current.and_then(|id| change.revision(id))
        && revision.head.as_str() == f.revision.head
        && let Some(held) = train.held(change.id, revision.id)
    {
        f.train.held = Some((f.revision.head.clone(), format!("{held:?}")));
        // GitHub's test merge only comes back once the head moves: while it's
        // stale, the train's empty merge of this head onto trunk as it is now
        // says what that merge would.
        if f.empty.is_none()
            && train
                .empty_on(change.id, revision.id)
                .is_some_and(|trunk| f.base_tip.as_deref() == Some(trunk.as_str()))
        {
            f.empty = Some(true);
        }
    }
    if let Some((index, member)) = train
        .joined()
        .iter()
        .enumerate()
        .find(|(_, m)| m.change == change.id)
    {
        let head = change
            .revision(member.revision)
            .expect("joined revision")
            .head
            .to_string();
        f.train.seat = Some(Seat {
            head,
            ahead: index as u32,
            candidate: train
                .in_flight()
                .and_then(|id| ledger.candidate(id))
                .map(|c| Candidate {
                    id: c.id.to_string(),
                    contains: c.changes.iter().any(|m| m.change == change.id),
                    landing: c.state == conary_core::ledger::State::Green,
                    peers: c
                        .changes
                        .iter()
                        .filter(|m| m.change != change.id)
                        .filter_map(|m| ledger.change(m.change).map(|c| c.pr))
                        .collect(),
                }),
        });
    }
}

pub(crate) fn issuances(f: &mut Facts, memory: &Memory) {
    let mut judges = memory.judge_issuance.clone();
    f.judgment = super::state::issuance(
        &mut judges,
        &f.revision,
        f.judgment_generation,
        f.judgment.as_ref().map(Judged::result),
    )
    .map(|result| Judged::from_result(f.revision.clone(), result));
    f.judgment_generation = judges.get(&f.revision).map(|(generation, _)| *generation);
    let mut majors = memory.major_issuance.clone();
    f.major = super::state::issuance(&mut majors, &f.revision, f.major_generation, f.major);
    f.major_generation = majors.get(&f.revision).map(|(generation, _)| *generation);
}
