//! Project validated worker results into the decision engine's service facts.
use super::jobs::{self, Completion, Purpose};
use super::persist::Result;
use super::*;
use conary_core::plugin;

pub(crate) fn hydrate(f: &mut Facts, record: &Record, now: u64, dry_run: bool) -> Result<()> {
    for work in &record.jobs {
        let completion: Completion = if dry_run {
            jobs::inspect(work, now)?
        } else {
            jobs::completion(work, now)?
        };
        let mut job = work.job.clone();
        if std::fs::read(work.path.with_extension("cancel"))
            .ok()
            .as_deref()
            == Some(b"obsolete\n")
            && completion.result.is_some()
        {
            f.stopped_jobs.insert(job.id.clone());
            continue;
        }
        job.state = match &completion.result {
            None => JobState::Running,
            Some(Ok(_)) => JobState::Done,
            Some(Err(_)) => JobState::Broken,
        };
        match &work.purpose {
            Purpose::Review(role) => {
                let reviewer = f.reviewers.entry(role.clone()).or_default();
                if work.posted {
                    continue;
                }
                if completion.result.is_none() {
                    reviewer.queued = true;
                    reviewer.under_way = Some(job.revision);
                } else if let Some(Ok(plugin::Response {
                    answer: plugin::Answer::Review { findings },
                    ..
                })) = &completion.result
                {
                    reviewer.answer = Some(Review {
                        revision: job.revision,
                        at: completion.at,
                        verdict: if findings.is_empty() {
                            Verdict::Clean
                        } else {
                            Verdict::Findings
                        },
                    });
                } else if job.revision == f.revision {
                    reviewer.failed_at = Some(completion.at);
                }
            }
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
            // and any other answer is a preparation that failed.
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
/// A fix's answer, each of its answers on the target of the finding it was
/// handed. An answer to anything it wasn't handed is invalid.
fn fixed(
    work: &jobs::Work,
    pushed: Option<String>,
    answers: Option<&[plugin::FindingAnswer]>,
) -> FixResult {
    let mut targets = Vec::new();
    for answer in answers.unwrap_or_default() {
        match work.handed.get(answer.problem) {
            Some(Some(handed)) => targets.push((handed.target.clone(), answer.fixed)),
            _ => return FixResult::Invalid,
        }
    }
    FixResult::Answer(FixAnswer {
        pushed,
        answers: targets,
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
