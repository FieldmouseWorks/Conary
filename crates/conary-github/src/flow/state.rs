use super::*;
use conary_core::Class;

/// Decide one intention. Memory records observations only: queuing, posting,
/// starting jobs and landing are never assumed to have succeeded here.
pub fn next(f: &Facts, memory: &Memory, now: u64) -> (Next, Memory) {
    let mut m = memory.clone();
    m.raised.extend(f.classified.raised.iter().cloned());
    let mut raised = f.clone();
    raised.classified = raised
        .classified
        .clone()
        .raise(m.raised.iter().cloned().collect());
    for claim in &f.claims {
        let entry = raised
            .jobs
            .entry(claim.kind)
            .or_insert_with(|| claim.job.clone());
        if claim.job.started >= entry.started {
            *entry = claim.job.clone();
        }
    }
    for id in &m.stopping {
        let ended = f
            .jobs
            .values()
            .chain(f.claims.iter().map(|c| &c.job))
            .chain(f.proof_data.records.iter().map(|r| &r.job))
            .chain(f.proof_data.live.iter().map(|h| &h.job))
            .chain(f.proof_data.prepared.iter().map(|p| &p.job))
            .any(|j| j.id == *id && !j.state.live());
        if ended || f.stopped_jobs.contains(id) {
            m.retired_jobs.insert(id.clone());
        }
    }
    m.retired_jobs.extend(f.stopped_jobs.iter().cloned());
    m.stopping.retain(|id| !m.retired_jobs.contains(id));
    m.answered_jobs.extend(m.retired_jobs.iter().cloned());
    raised.jobs.retain(|_, j| !m.retired_jobs.contains(&j.id));
    raised.working.retain(|id| !m.retired_jobs.contains(id));
    raised
        .proof_data
        .records
        .retain(|r| !m.retired_jobs.contains(&r.job.id));
    raised
        .proof_data
        .live
        .retain(|h| !m.retired_jobs.contains(&h.job.id));
    raised
        .proof_data
        .prepared
        .retain(|p| !m.retired_jobs.contains(&p.job.id));
    if let Some(record) = raised
        .proof_data
        .records
        .iter()
        .max_by_key(|r| r.job.started)
    {
        raised.jobs.insert(Kind::Proof, record.job.clone());
    }
    if m.judge_issuance
        .keys()
        .any(|rev| rev.head == raised.revision.head && rev.base == raised.revision.base)
    {
        raised.judge_asked = true;
    }
    raised.judgment = issuance(
        &mut m.judge_issuance,
        &raised.revision,
        raised.judgment_generation,
        raised.judgment.as_ref().map(Judged::result),
    )
    .map(|result| Judged::from_result(raised.revision.clone(), result));
    judge_answered(&mut raised, &mut m);
    raised.major = issuance(
        &mut m.major_issuance,
        &raised.revision,
        raised.major_generation,
        raised.major,
    );
    raised.major_generation = m
        .major_issuance
        .get(&raised.revision)
        .map(|(generation, _)| *generation);
    let f = &raised;
    if m.head != f.revision.head {
        m.head = f.revision.head.clone();
        m.seen = now;
        m.no_checks_since = None;
        m.strikes.clear();
        m.retry = None;
        m.handed.clear();
        m.recovered.clear();
        // Lost findings survive pushes, indexed by base; old judgments do
        // not, since their Revision must match below.
    }
    for (kind, clear) in [
        // A train PR's candidate decides its emptiness, not GitHub's preview.
        (
            Failure::Empty,
            f.empty.is_some() || super::decide::by_train(f),
        ),
        (
            Failure::Mergeable,
            !matches!(f.merging, Merge::Unknown | Merge::Refused),
        ),
    ] {
        if clear && f.failed != Some(kind) {
            m.strikes.remove(&kind);
            if m.retry.is_some_and(|(k, _)| k == kind) {
                m.retry = None;
            }
        }
    }
    if f.succeeded.contains(&Failure::Update) && f.failed != Some(Failure::Update) {
        m.update_requested = Some((f.revision.head.clone(), now));
    }
    for kind in &f.succeeded {
        if f.failed != Some(*kind) {
            m.strikes.remove(kind);
            if m.retry.is_some_and(|(k, _)| k == *kind) {
                m.retry = None;
            }
        }
    }
    for (kind, job) in &f.jobs {
        if kind.fixes_branch() {
            m.handed.insert(*kind, job.revision.clone());
        }
    }
    let decision = super::decide::decide(f, &mut m, now);
    if let Step::StopJob(id) = &decision.1 {
        m.stopping.insert(id.clone());
    }
    let decision = if decision.1 == Step::UpdateBranch {
        if let Some((head, at)) = m
            .update_requested
            .as_ref()
            .filter(|(head, at)| head == &f.revision.head && now < at.saturating_add(600))
        {
            let _ = head;
            (
                decision.0,
                Step::Wait {
                    until: Some(at.saturating_add(600)),
                },
                "GitHub is still updating this head with its base".into(),
            )
        } else {
            decision
        }
    } else {
        decision
    };
    let (stage, mut step, description) = super::train::apply(f, decision);
    let mut description = description.chars().take(140).collect::<String>();
    let no_reviewer = super::review::overdue(f, &m, now);
    let red = f.checks.iter().any(|c| c.state == Ci::Red);
    let own_conflict = f.merging == Merge::Conflict
        && f.bot_edited == Some(true)
        && f.major != Some(Major::Held)
        && !(f.renovate
            && f.dependency.is_some()
            && (red || f.dependency == Some(Dependency::Major) && f.major != Some(Major::Cleared)));
    let fixed_renovate = own_conflict
        || f.dependency == Some(Dependency::Minor)
            && (f.bot_edited == Some(true) || f.gate_edited)
            && f.classified.class != Class::R2
            && !red;
    let owner_notice = if f.draft
        || f.paused.is_some()
        || !f.open
        || f.labels.contains(&f.settings.hold_label)
        || f.stacked_on.is_some()
        || matches!(step, Step::ListOwner)
    {
        None
    } else if no_reviewer
        && stage == Stage::Review
        && matches!(step, Step::Wait { .. } | Step::AskReview(_))
    {
        Some(OwnerNotice::NoReviewer)
    } else if f
        .created_at
        .is_some_and(|at| now > at.saturating_add(86400))
        && (!f.renovate || fixed_renovate)
        && !matches!(
            step,
            Step::Merge | Step::JoinTrain | Step::CloseAlreadyLanded
        )
    {
        Some(OwnerNotice::OpenOverDay)
    } else {
        None
    };
    // A pause is the owner's switch, never a stall.
    let watched = owner_notice.is_none()
        && !(f.draft
            || f.paused.is_some()
            || !f.open
            || (f.stacked_on.is_some() && stage == Stage::Base)
            || matches!(step, Step::ListOwner));
    let mut queued = None;
    let mut recovery = None;
    let overdue_at = if watched {
        let duration = if f.labels.contains(&f.settings.hold_label) {
            f.settings.hold_stall_after
        } else if stage == Stage::Train {
            f.settings.train_stall_after
        } else {
            f.settings.stall_after.at(f.rank().level)
        };
        let failure = m
            .strikes
            .iter()
            .find(|(_, n)| **n >= 3)
            .map(|(kind, _)| *kind)
            .or_else(|| m.retry.map(|(kind, _)| kind));
        let since = m
            .wait
            .as_ref()
            .filter(|w| {
                w.head == f.revision.head
                    && w.base == f.revision.base
                    && ((failure.is_some() && w.failure == failure)
                        || (w.stage == stage
                            && w.description == description
                            && w.failure == failure))
            })
            .map_or(now, |w| w.since);
        // A check stalled on its deadlines, when that's what was decided,
        // is overdue at once, as three strikes are.
        let stalled = super::proofs::stalled(f).is_some_and(|s| {
            matches!(s, super::proofs::Stall::Twice { .. })
                && description == s.to_string().chars().take(140).collect::<String>()
        });
        let failing = m.strikes.values().any(|n| *n >= 3) || stalled;
        // A wait on CI counts from its runs' start, never their queue.
        let clock = if stage == Stage::Ci && !failing {
            super::ci::clock(&f.checks)
        } else {
            super::ci::Clock::Wait
        };
        queued = match clock {
            super::ci::Clock::Queued(at) => Some(at.unwrap_or(since)),
            _ => None,
        };
        let since = match clock {
            super::ci::Clock::Started(at) => since.max(at),
            _ => since,
        };
        let due = if queued.is_some() {
            None
        } else if failing {
            Some(since)
        } else {
            let refreshing = f.renovate
                && f.settings.renovate_sweep
                && !f.labels.contains(&f.settings.hold_label)
                && (f.checks.iter().any(|c| c.state == Ci::Red)
                    || f.rebase_requested
                    || f.rebase_removed
                        .iter()
                        .copied()
                        .chain(f.timeline_cut_at)
                        .any(|t| now < t.saturating_add(f.settings.renovate_run)));
            let drain = 4 * (3600 + f.settings.renovate_run).max(600) + f.settings.renovate_run;
            let grace = if refreshing {
                if f.release_at.is_some() {
                    3 * (21600 + 3600 + f.settings.renovate_run) + drain
                } else {
                    drain
                }
            } else {
                0
            };
            let until = if refreshing {
                f.release_at.unwrap_or(0)
            } else {
                0
            };
            Some(
                since
                    .max(until)
                    .saturating_add(grace)
                    .saturating_add(duration),
            )
        };
        let wait = Wait {
            head: f.revision.head.clone(),
            base: f.revision.base.clone(),
            stage,
            description: description.clone(),
            failure,
            since,
            due: due.unwrap_or(u64::MAX),
        };
        // Past its deadline, a wait is brought back as GitHub allows, once
        // in its stall. The wait stays what was decided, so the stall's
        // clock runs on whatever the status says.
        if !failing && let Some(look) = super::recover::look(f, &m, stage, &step, &wait, now) {
            // A fix still to hand says so; the pass shows the recovery if
            // it can't hand it after all.
            if let Some(ask) = look.step {
                step = ask;
                description = look.recovery.describe();
            } else if let Step::Wait { .. } = step {
                description = look.recovery.describe();
            }
            recovery = Some(look.recovery);
        }
        m.wait = Some(wait);
        due
    } else {
        m.wait = None;
        None
    };
    (
        Next {
            stage: if f.draft || f.paused.is_some() || f.labels.contains(&f.settings.hold_label) {
                Stage::None
            } else {
                stage
            },
            step: step.clone(),
            description,
            wait_until: match step {
                Step::Wait { until } => until,
                Step::Retry { at, .. } => Some(at),
                _ => None,
            },
            overdue_at,
            queued,
            recovery,
            owner_notice,
            wait_on: watched.then(|| {
                if f.labels.contains(&f.settings.hold_label) {
                    WaitOn::Hold
                } else if stage == Stage::Train {
                    WaitOn::Train
                } else if f.settings.renovate_sweep
                    && ((f.major_generation.is_some()
                        && f.major.is_none()
                        && stage == Stage::Review
                        && matches!(step, Step::Wait { .. }))
                        || (f.renovate
                            && f.dependency.is_some()
                            && (f.merging != Merge::Conflict || f.bot_edited == Some(true))
                            && (f.checks.iter().any(|c| c.state == Ci::Red)
                                || f.dependency == Some(Dependency::Major) && f.major.is_none())))
                {
                    WaitOn::Sweep
                } else {
                    WaitOn::Steward
                }
            }),
            on_usage: watched
                && !m.strikes.values().any(|n| *n >= 3)
                && f.jobs.iter().any(|(kind, j)| {
                    matches!(j.state, JobState::Running | JobState::Queued) && *kind != Kind::Proof
                }),
            proof: if matches!(step, Step::PostProof) {
                super::proofs::posting(f)
            } else {
                None
            }
            .or_else(|| {
                if matches!(step, Step::Prepare | Step::Prove | Step::PostProof) {
                    super::proofs::next(f, now).and_then(|(_, _, _, plan)| plan)
                } else {
                    None
                }
            }),
            // The pass names them: some live in its configuration alone.
            switches: Vec::new(),
            priority: f.rank().level,
            degraded: Vec::new(),
        },
        m,
    )
}

/// Whether the finished judge job on this revision was answered, against
/// every issuance the flow knows for it, GitHub's and remembered. A job is
/// answered once an issuance seals what it asked on (any issuance, for a
/// request from before the flow said), and stays answered: its answer is
/// then superseded, never missing, so it leaves the unanswered count. A
/// finished job is owed its producer's answer only while no issuance of its
/// revision is known, not merely while GitHub shows none.
fn judge_answered(f: &mut Facts, m: &mut Memory) {
    let Some(job) = f.jobs.get_mut(&Kind::Judge) else {
        return;
    };
    let Some(asked) = f.judge_requests.get(&job.id) else {
        return;
    };
    if job.revision != f.revision || !matches!(job.state, JobState::Done | JobState::Unknown) {
        return;
    }
    let seals = |asked: &Option<Basis>| {
        f.judgment
            .as_ref()
            .is_some_and(|j| asked.is_none() || j.basis == *asked)
    };
    if m.answered_judges.contains(&job.id) || seals(asked) {
        m.answered_judges.insert(job.id.clone());
        m.lost_jobs.remove(&job.id);
        for ended in m.unanswered.values_mut() {
            ended.remove(&job.id);
        }
        m.unanswered.retain(|_, ended| !ended.is_empty());
        job.state = JobState::Answered;
    } else if m.judge_issuance.contains_key(&f.revision) {
        job.state = JobState::Done;
    }
}

/// A deleted/edited comment cannot roll an observed decision back. Conflicting
/// words at the same generation stay blocked until a strictly newer issuance.
pub(super) fn issuance<T: Clone + Eq>(
    receipts: &mut std::collections::BTreeMap<Revision, (u64, Option<T>)>,
    revision: &Revision,
    generation: Option<u64>,
    word: Option<T>,
) -> Option<T> {
    if let Some(generation) = generation {
        let receipt = receipts
            .entry(revision.clone())
            .or_insert((generation, word.clone()));
        if generation > receipt.0 {
            *receipt = (generation, word);
        } else if generation == receipt.0 && receipt.1 != word {
            receipt.1 = None;
        }
    }
    // An absent or older observation leaves the authenticated decision in
    // place. A conflict/unsupported word is a tombstone until a new issuance.
    receipts.get(revision).and_then(|(_, word)| word.clone())
}
