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
                        answer:
                            plugin::Answer::Fix {
                                result: plugin::FixResult::Pushed { head },
                            },
                        ..
                    })) => FixResult::Answer(FixAnswer {
                        pushed: Some(head.clone()),
                        threads: Default::default(),
                        description: None,
                        comment: false,
                    }),
                    Some(Ok(_)) => FixResult::Missing,
                    Some(Err(_)) => FixResult::Invalid,
                    None => FixResult::Missing,
                };
                f.jobs.insert(*kind, job.clone());
                f.claims.push(FixClaim {
                    kind: *kind,
                    job,
                    answer,
                    posted: work.posted,
                });
            }
            Purpose::Judge => {
                f.jobs.insert(Kind::Judge, job.clone());
                if job.revision == f.revision
                    && let Some(Ok(plugin::Response {
                        answer: plugin::Answer::Judge { verdict, .. },
                        ..
                    })) = &completion.result
                {
                    let verdict = match verdict {
                        plugin::Judgment::Pass => Judgment::Land,
                        plugin::Judgment::Fail => Judgment::Failing,
                        plugin::Judgment::Owner => Judgment::Owner,
                    };
                    match f.judgment_generation {
                        Some(generation) if generation > work.generation => {}
                        Some(generation) if generation == work.generation => {
                            if f.judgment.as_ref().map(|j| j.verdict) != Some(verdict) {
                                f.judgment = None;
                            }
                        }
                        _ => {
                            f.judgment = Some(Judged {
                                revision: job.revision,
                                verdict,
                            });
                            f.judgment_generation = Some(work.generation);
                        }
                    }
                }
            }

            Purpose::Proof => {
                if job.revision == f.revision
                    && work.recipes == super::pass::recipes(f)
                    && matches!(completion.result, Some(Ok(_)))
                {
                    // v1 identity is an assertion, not an evidence attestation.
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
        }
    }
    Ok(())
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
