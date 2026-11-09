//! Fix answers are local observations, but their claimed push and thread
//! membership must agree with the GitHub facts before anything is posted.
use super::*;
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct FixClaim {
    pub kind: Kind,
    pub job: Job,
    pub answer: FixResult,
    /// The preceding executor confirmed that all answers went up.
    pub posted: bool,
}
#[derive(Debug, Clone, Default)]
pub enum FixResult {
    #[default]
    Missing,
    Invalid,
    Answer(FixAnswer),
}
#[derive(Debug, Clone)]
pub struct FixAnswer {
    pub pushed: Option<String>,
    pub threads: BTreeMap<String, bool>,
    /// Fingerprint of the replacement description, never its prompt text.
    pub description: Option<String>,
    pub comment: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unposted {
    Missing,
    Invalid,
    Unpushed,
    Moved,
}

pub(super) fn look(f: &Facts, m: &mut Memory, now: u64) -> Option<super::decide::Decision> {
    for claim in &f.claims {
        let job = &claim.job;
        if m.answered_jobs.contains(&job.id) {
            continue;
        }
        let moved = job.revision.head != f.revision.head;
        if job.state.live() {
            if moved
                && (job.state == JobState::Queued
                    || f.settings
                        .job_login
                        .as_ref()
                        .zip(f.pushes.get(&f.revision.head))
                        .is_some_and(|(expected, who)| who != expected))
            {
                return Some(super::decide::at(
                    Stage::Fix,
                    Step::StopJob(job.id.clone()),
                    "Someone else moved the branch past this fix job",
                ));
            }
            if moved && f.settings.job_login.is_some() && !f.pushes.contains_key(&f.revision.head) {
                let (looks, at) = m
                    .push_looks
                    .entry((job.id.clone(), f.revision.head.clone()))
                    .or_insert((0, 0));
                if *looks < 3 && now >= *at {
                    *looks += 1;
                    *at = now.saturating_add(120);
                }
                if *looks < 3 {
                    return Some(super::decide::wait(
                        Stage::Fix,
                        Some(*at),
                        "GitHub has not identified who pushed this head yet",
                    ));
                }
            }
            continue;
        }
        if claim.posted {
            m.answered_jobs.insert(job.id.clone());
            m.strikes.remove(&Failure::Answer);
            return Some(super::decide::wait(
                Stage::Fix,
                Some(now),
                "A fix job's answers went up; read the PR again",
            ));
        }
        let answer = match &claim.answer {
            FixResult::Missing => {
                m.unposted
                    .insert(claim.kind, (f.revision.head.clone(), Unposted::Missing));
                m.answered_jobs.insert(job.id.clone());
                continue;
            }
            FixResult::Invalid => {
                m.unposted
                    .insert(claim.kind, (f.revision.head.clone(), Unposted::Invalid));
                m.answered_jobs.insert(job.id.clone());
                continue;
            }
            FixResult::Answer(answer) => answer,
        };
        let expected = answer.pushed.as_ref().unwrap_or(&job.revision.head);
        let fixed_without_push = answer.threads.values().any(|fixed| *fixed)
            && answer
                .pushed
                .as_ref()
                .is_none_or(|p| p == &job.revision.head);
        let bad_commit = answer.pushed.as_ref().is_some_and(|p| {
            p.len() != 40
                || !p
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        });
        let unrelated = answer.threads.keys().any(|id| !f.threads.contains_key(id));
        let changed_description = answer.description.as_ref().is_some_and(|description| {
            f.revision.description != job.revision.description
                && &f.revision.description != description
        });
        let reason = if fixed_without_push || bad_commit || unrelated {
            Some(Unposted::Invalid)
        } else if !f.open || changed_description {
            Some(Unposted::Moved)
        } else if expected != &f.revision.head {
            Some(if !moved {
                Unposted::Unpushed
            } else {
                Unposted::Moved
            })
        } else {
            None
        };
        if let Some(reason) = reason {
            m.unposted
                .insert(claim.kind, (f.revision.head.clone(), reason));
            m.answered_jobs.insert(job.id.clone());
            continue;
        }
        if answer.threads.is_empty() && answer.description.is_none() && !answer.comment {
            m.answered_jobs.insert(job.id.clone());
            continue;
        }
        return Some(super::decide::at(
            Stage::Fix,
            Step::PostFix(job.id.clone()),
            "Post the fix job's answers on the head GitHub actually has",
        ));
    }
    None
}
