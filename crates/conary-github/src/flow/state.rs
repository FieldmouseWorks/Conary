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
    if let Some(record) = f.proof_data.records.iter().max_by_key(|r| r.job.started) {
        raised.jobs.insert(Kind::Proof, record.job.clone());
    }
    for id in &m.stopping {
        let ended = f
            .jobs
            .values()
            .chain(f.claims.iter().map(|c| &c.job))
            .chain(f.proof_data.records.iter().map(|r| &r.job))
            .chain(f.proof_data.live.iter().map(|h| &h.job))
            .chain(f.proof_data.inputs_job.iter().map(|h| &h.job))
            .any(|j| j.id == *id && !j.state.live());
        if ended || f.stopped_jobs.contains(id) {
            m.retired_jobs.insert(id.clone());
        }
    }
    m.retired_jobs.extend(f.stopped_jobs.iter().cloned());
    m.stopping.retain(|id| !m.retired_jobs.contains(id));
    m.answered_jobs.extend(m.retired_jobs.iter().cloned());
    raised.jobs.retain(|_, j| !m.retired_jobs.contains(&j.id));
    raised
        .proof_data
        .records
        .retain(|r| !m.retired_jobs.contains(&r.job.id));
    raised
        .proof_data
        .live
        .retain(|h| !m.retired_jobs.contains(&h.job.id));
    if raised
        .proof_data
        .inputs_job
        .as_ref()
        .is_some_and(|j| m.retired_jobs.contains(&j.job.id))
    {
        raised.proof_data.inputs_job = None;
    }
    let f = &raised;
    if m.head != f.revision.head {
        m.head = f.revision.head.clone();
        m.seen = now;
        m.no_checks_since = None;
        m.strikes.clear();
        m.retry = None;
        m.handed.clear();
        // Lost findings survive pushes, indexed by base; old judgments do
        // not, since their Revision must match below.
    }
    for (kind, clear) in [
        (Failure::Empty, f.empty.is_some()),
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
    for (role, reviewer) in &f.reviewers {
        if reviewer.record_id.is_some()
            && reviewer.record_id != m.latest_reviews.get(role).copied()
            && reviewer
                .review
                .as_ref()
                .is_some_and(|v| v.revision == f.revision)
            && f.failed_review.as_ref().map(|p| &p.role) != Some(role)
            && reviewer.failed_at.is_none()
        {
            m.review_failures
                .remove(&(role.clone(), f.revision.clone()));
        }
        if let Some(at) = reviewer
            .failed_at
            .into_iter()
            .chain(reviewer.queue_failed_at)
            .max()
        {
            let failure = m
                .review_failures
                .entry((role.clone(), f.revision.clone()))
                .or_insert((0, u64::MAX));
            if failure.1 != at {
                failure.0 = failure.0.saturating_add(1);
                failure.1 = at;
            }
        }
        if reviewer.queued {
            m.missing_reviews
                .remove(&(role.clone(), f.revision.clone()));
        }
    }
    if let Some(failed) = &f.failed_review
        && let Some(answer) = f
            .reviewers
            .get(&failed.role)
            .and_then(|r| r.answer.as_ref())
    {
        if !failed.unavailable {
            let tries = m
                .review_posts
                .entry((failed.role.clone(), answer.revision.clone(), answer.at))
                .or_default();
            *tries = tries.saturating_add(1);
            if answer.revision.head == f.revision.head && answer.revision.base == f.revision.base {
                let failure = m
                    .review_failures
                    .entry((failed.role.clone(), f.revision.clone()))
                    .or_insert((0, 0));
                failure.0 = failure.0.saturating_add(1);
                failure.1 = now;
            }
        }
        let strikes = m.strikes.entry(Failure::Post).or_default();
        *strikes = strikes.saturating_add(1);
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
    let (stage, step, description) = super::train::apply(f, decision);
    let description = description.chars().take(140).collect::<String>();
    let no_reviewer = f.classified.review()
        && f.reviewers.values().all(|r| {
            r.unavailable_until.is_some_and(|until| until > now)
                && !r
                    .review
                    .as_ref()
                    .is_some_and(|r| r.revision == f.revision && r.verdict == Verdict::Clean)
        });
    let failures = m.review_failures.iter().any(|((role, rev), (count, _))| {
        rev == &f.revision
            && *count >= 3
            && f.reviewers
                .get(role)
                .is_some_and(|r| r.unavailable_until.is_none_or(|until| until <= now))
    });
    let unposted = m.review_posts.iter().any(|(key, count)| {
        *count >= 3 && !m.consumed_reviews.contains(key) && f.reviewers.contains_key(&key.0)
    });
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
        || !f.open
        || f.labels.contains(&f.settings.hold_label)
        || f.stacked_on.is_some()
        || matches!(step, Step::ListOwner)
    {
        None
    } else if no_reviewer
        && matches!(stage, Stage::Reviewer | Stage::Review)
        && matches!(step, Step::Wait { .. })
    {
        Some(OwnerNotice::NoReviewer)
    } else if failures && f.classified.review() {
        Some(OwnerNotice::ReviewFailures)
    } else if unposted {
        Some(OwnerNotice::UnpostedReview)
    } else if f
        .created_at
        .is_some_and(|at| now > at.saturating_add(86400))
        && (!f.renovate || fixed_renovate)
        && !matches!(
            step,
            Step::Merge | Step::Settle | Step::JoinTrain | Step::CloseAlreadyLanded
        )
    {
        Some(OwnerNotice::OpenOverDay)
    } else {
        None
    };
    let watched = owner_notice.is_none()
        && !(f.draft
            || !f.open
            || (f.stacked_on.is_some() && stage == Stage::Base)
            || matches!(step, Step::ListOwner));
    let overdue_at = if watched {
        let duration = if f.labels.contains(&f.settings.hold_label) {
            f.settings.hold_stall_after
        } else if stage == Stage::Train {
            f.settings.train_stall_after
        } else if stage == Stage::Merge && f.renovate && f.bot_merges && f.bot_automerge_enabled {
            f.settings.renovate_merge_after
        } else {
            f.settings.stall_after
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
        let failing = m.strikes.values().any(|n| *n >= 3);
        let due = if failing {
            since
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
            since
                .max(until)
                .saturating_add(grace)
                .saturating_add(duration)
        };
        m.wait = Some(Wait {
            head: f.revision.head.clone(),
            base: f.revision.base.clone(),
            stage,
            description: description.clone(),
            failure,
            since,
            due,
        });
        Some(due)
    } else {
        m.wait = None;
        None
    };
    (
        Next {
            stage: if f.draft || f.labels.contains(&f.settings.hold_label) {
                Stage::None
            } else {
                stage
            },
            step: step.clone(),
            description,
            overdue_at,
            owner_notice,
            wait_on: watched.then(|| {
                if f.labels.contains(&f.settings.hold_label) {
                    WaitOn::Hold
                } else if stage == Stage::Train {
                    WaitOn::Train
                } else if f.renovate
                    && f.bot_merges
                    && f.bot_automerge_enabled
                    && stage == Stage::Merge
                {
                    WaitOn::Renovate
                } else if f.renovate
                    && f.dependency.is_some()
                    && (f.merging != Merge::Conflict || f.bot_edited == Some(true))
                    && (f.checks.iter().any(|c| c.state == Ci::Red)
                        || f.dependency == Some(Dependency::Major) && f.major.is_none())
                {
                    WaitOn::Sweep
                } else {
                    WaitOn::Steward
                }
            }),
            on_usage: watched
                && !m.strikes.values().any(|n| *n >= 3)
                && f.jobs.iter().any(|(kind, j)| {
                    matches!(j.state, JobState::Running | JobState::Queued)
                        && (*kind != Kind::Proof || super::proofs::engine_job(f, &j.id))
                }),
            proof: if matches!(step, Step::PostProof) {
                super::proofs::posting(f)
            } else {
                None
            }
            .or_else(|| {
                if matches!(step, Step::Prove | Step::PostProof) {
                    super::proofs::next(f, now).and_then(|(_, _, _, plan)| plan)
                } else {
                    None
                }
            }),
        },
        m,
    )
}
