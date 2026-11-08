use super::*;
use conary_core::Class;

/// Decide one intention. Memory records observations only: queuing, posting,
/// starting jobs and landing are never assumed to have succeeded here.
pub fn next(f: &Facts, memory: &Memory, now: u64) -> (Next, Memory) {
    let mut m = memory.clone();
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
    let (stage, step, description) = decide(f, &mut m, now);
    let description = description.chars().take(140).collect::<String>();
    let watched = !(f.draft
        || !f.open
        || (f.stacked_on.is_some() && stage == Stage::Base)
        || matches!(step, Step::ListOwner));
    let overdue_at = if watched {
        let duration = if f.labels.contains("hold") {
            f.settings.hold_stall_after
        } else if stage == Stage::Train {
            f.settings.train_stall_after
        } else if stage == Stage::Merge && f.renovate && f.bot_merges {
            f.settings.renovate_merge_after
        } else {
            f.settings.stall_after
        };
        let failure = m.retry.map(|(kind, _)| kind);
        let since = m
            .wait
            .as_ref()
            .filter(|w| {
                w.head == f.revision.head
                    && w.base == f.revision.base
                    && ((failure.is_some() && w.failure == failure)
                        || (w.stage == stage && w.description == description))
            })
            .map_or(now, |w| w.since);
        let failing = m.strikes.values().any(|n| *n >= 3);
        let due = if failing {
            since
        } else {
            let until = match step {
                Step::Wait { until: Some(until) } => until,
                _ => since,
            };
            since.max(until).saturating_add(duration)
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
            stage,
            step,
            description,
            overdue_at,
        },
        m,
    )
}

type Decision = (Stage, Step, String);
fn at(stage: Stage, step: Step, description: &str) -> Decision {
    (stage, step, description.into())
}
fn wait(stage: Stage, until: Option<u64>, why: &str) -> Decision {
    at(stage, Step::Wait { until }, why)
}

fn decide(f: &Facts, m: &mut Memory, now: u64) -> Decision {
    if !f.open {
        return wait(Stage::None, None, "PR is closed");
    }
    if f.draft {
        return wait(
            Stage::None,
            None,
            "Draft: work in progress, reviewed once marked ready",
        );
    }
    if !f.trusted {
        return at(
            Stage::Owner,
            Step::ListOwner,
            "Outside contribution: the owner reviews and merges it",
        );
    }
    if !f.complete {
        return at(
            Stage::Owner,
            Step::ListOwner,
            "Too much to read: the owner merges it",
        );
    }
    if f.base_held {
        return wait(Stage::Base, None, "Waiting for the default branch's CI");
    }
    if f.labels.contains("hold") {
        return wait(Stage::None, None, "On hold (the hold label)");
    }
    if f.stacked_on.is_some() {
        return wait(
            Stage::Base,
            None,
            "Stacked on an open PR: waiting to be retargeted",
        );
    }
    if f.labels.contains("needs-peter") {
        if judgment(f) == Some(Judgment::Owner) {
            m.owner_saw = Some(f.revision.clone());
        }
        return at(
            Stage::Owner,
            Step::ListOwner,
            "The owner decides (the needs-peter label)",
        );
    }
    if let Some(kind) = f.failed {
        let times = m.strikes.entry(kind).or_default();
        *times = times.saturating_add(1);
        let delay = (1u64 << (*times - 1).min(6)).min(60) * 60;
        let due = now.saturating_add(delay);
        m.retry = Some((kind, due));
        return at(
            failure_stage(kind),
            Step::Retry { kind, at: due },
            "The step failed; trying again later",
        );
    }
    if let Some((kind, due)) = m.retry {
        if now < due {
            return at(
                failure_stage(kind),
                Step::Retry { kind, at: due },
                "Waiting to try the failed step again",
            );
        }
        m.retry = None;
    }
    if f.merging == Merge::Conflict {
        if f.dependency.is_some() && f.bot_edited != Some(true) {
            return wait(
                Stage::Fix,
                None,
                "Conflicts with its base; its bot rebases it",
            );
        }
        if f.major == Some(Major::Held) {
            return at(
                Stage::Owner,
                Step::ListOwner,
                "Major update held for the owner",
            );
        }
        if f.renovate
            && f.dependency.is_some()
            && (ci(f, m, now) == Ci::Red
                || f.dependency == Some(Dependency::Major) && f.major != Some(Major::Cleared))
        {
            return wait(
                Stage::Fix,
                None,
                "Conflicts with its base; dependency sweep has it",
            );
        }
        return hand(f, m, now, Kind::Conflict, false);
    }
    let ci = ci(f, m, now);
    let edited = f.bot_edited.unwrap_or(!f.renovate || f.settings.train) || f.gate_edited;
    let fixed =
        f.dependency == Some(Dependency::Minor) && edited && f.classified.class != Class::R2;
    if f.dependency.is_some() && !fixed {
        let threads = f.open_threads > 0 || f.changes_requested;
        if f.release_at.is_some_and(|t| t > now) && !threads {
            return wait(
                Stage::ReleaseAge,
                f.release_at,
                "Lock file refresh: nothing old enough yet",
            );
        }
        if f.renovate && ci == Ci::Red {
            return wait(
                Stage::Fix,
                None,
                "Dependency update, CI red; dependency sweep has it",
            );
        }
        if f.major == Some(Major::Held) {
            return at(
                Stage::Owner,
                Step::ListOwner,
                "Major update held for the owner",
            );
        }
        if f.renovate && f.dependency == Some(Dependency::Major) && f.major.is_none() {
            return wait(
                if threads { Stage::Fix } else { Stage::Review },
                None,
                "Major update: waiting for the dependency sweep's verdict on this head",
            );
        }
        if threads {
            return hand(f, m, now, Kind::Findings, true);
        }
        if ci == Ci::Red {
            return hand(f, m, now, Kind::Red, true);
        }
        return after_review(f, m, now, ci, false);
    }
    // A newly posted review must get a fresh GitHub read before its findings
    // can be considered resolved. A stale clean verdict never covers this head.
    let capped = f.reviewers.values().map(|r| r.rounds).max().unwrap_or(0) >= f.settings.rounds;
    let needs_review = f.classified.review();
    let mut live = 0;
    let mut clean = true;
    let mut findings = f.open_threads > 0 || f.changes_requested;
    for (name, r) in &f.reviewers {
        let available = r.unavailable_until.is_none_or(|until| until <= now);
        let current = r.review.as_ref().filter(|v| v.revision == f.revision);
        let lost = m
            .lost_on
            .get(name)
            .and_then(|bases| bases.get(&f.revision.base));
        if let Some(answer) = &r.answer
            && answer.revision.base == f.revision.base
            && (answer.revision == f.revision || capped || !available || !needs_review)
        {
            return at(
                Stage::Review,
                Step::PostReview(name.clone()),
                "Posting the review that came back",
            );
        }
        if !available {
            if r.under_way {
                return wait(
                    Stage::Review,
                    r.unavailable_until,
                    "A reviewer out of service is still reviewing it",
                );
            }
            if lost.is_some_and(|h| *h == f.revision.head || capped || !needs_review) {
                return wait(
                    Stage::Reviewer,
                    r.unavailable_until,
                    "Lost findings are owed by a reviewer out of service",
                );
            }
            continue;
        }
        live += 1;
        if current.is_some_and(|v| v.verdict == Verdict::Clean) && lost.is_none() {
            continue;
        }
        clean = false;
        if current.is_some_and(|v| v.verdict == Verdict::Findings) {
            findings = true;
        }
        if (needs_review && !capped || lost.is_some()) && current.is_none() {
            if findings {
                return hand(f, m, now, Kind::Findings, true);
            }
            let due = m.seen.saturating_add(f.settings.settle_delay);
            if now < due {
                return wait(
                    Stage::Review,
                    Some(due),
                    "Waiting for the head to settle before review",
                );
            }
            return if r.queued {
                wait(Stage::Review, None, "Reviewing this commit")
            } else {
                at(
                    Stage::Review,
                    Step::QueueReview(name.clone()),
                    "Queue a review of this commit",
                )
            };
        }
        if capped && r.queued {
            return wait(
                Stage::Review,
                None,
                "Past the round limit; a queued review is still owed",
            );
        }
    }
    if findings {
        return hand(f, m, now, Kind::Findings, true);
    }
    if needs_review && live == 0 && !capped {
        return wait(Stage::Reviewer, None, "No reviewer can review it now");
    }
    if needs_review && !clean && !capped {
        let due = f
            .reviewers
            .values()
            .filter_map(|r| r.review.as_ref())
            .map(|r| r.at)
            .max()
            .unwrap_or(now)
            .saturating_add(f.settings.settle_delay);
        if now <= due {
            return wait(
                Stage::Review,
                Some(due),
                "Findings answered; waiting for another look",
            );
        }
        if let Some((name, _)) = f.reviewers.iter().find(|(_, r)| !r.queued) {
            return at(
                Stage::Review,
                Step::QueueReview(name.clone()),
                "Findings answered; another review round",
            );
        }
        return wait(Stage::Review, None, "Findings answered; reviewing again");
    }
    after_review(f, m, now, ci, capped && !clean)
}

fn ci(f: &Facts, m: &mut Memory, now: u64) -> Ci {
    if f.checks.iter().any(|c| c.state == Ci::Red) {
        return Ci::Red;
    }
    if f.checks.iter().any(|c| c.state == Ci::Pending) {
        return Ci::Pending;
    }
    if !f.checks.is_empty() {
        return Ci::Green;
    }
    if f.settings.train || !f.settings.require_ci {
        return Ci::Green;
    }
    let first = *m.no_checks_since.get_or_insert(now);
    if now < first.saturating_add(300) {
        Ci::Pending
    } else {
        Ci::None
    }
}

fn judgment(f: &Facts) -> Option<Judgment> {
    f.judgment
        .as_ref()
        .filter(|j| j.revision == f.revision)
        .map(|j| j.verdict)
}

fn after_review(f: &Facts, m: &mut Memory, now: u64, ci: Ci, capped: bool) -> Decision {
    if ci == Ci::Red || f.proof == Proof::Failed {
        return hand(f, m, now, Kind::Red, true);
    }
    let verdict = judgment(f);
    if matches!(verdict, Some(Judgment::Failing | Judgment::Misdescribed)) {
        return hand(f, m, now, Kind::JudgeFailure, false);
    }
    if verdict == Some(Judgment::Owner) && m.owner_saw.as_ref() != Some(&f.revision) {
        return at(
            Stage::Owner,
            Step::ListOwner,
            "The judge sent it to the owner",
        );
    }
    match f.proof {
        Proof::InLine(_) => {
            return wait(
                Stage::ProofSlot,
                None,
                "Settled; waiting its turn for machine proofs",
            );
        }
        Proof::Running => return wait(Stage::Proof, None, "Settled; machine proofs are running"),
        Proof::Unposted => {
            return at(
                Stage::Proof,
                Step::PostProof,
                "Posting machine proof verdicts",
            );
        }
        Proof::Needed if ci != Ci::Pending || !f.settings.proofs.is_empty() => {
            return work(f, m, now, Kind::Proof);
        }
        _ => {}
    }
    if ci == Ci::Pending {
        return wait(
            Stage::Ci,
            m.no_checks_since.map(|t| t.saturating_add(300)),
            "Settled; waiting for CI",
        );
    }
    let requires_judge = f.classified.class == Class::R2
        || f.needs_judge
        || f.unseen
        || ci == Ci::None
        || f.judge_asked
        || capped
        || f.dependency == Some(Dependency::Major) && !f.renovate;
    if requires_judge {
        if !matches!(verdict, Some(Judgment::Land))
            && !(verdict == Some(Judgment::Owner) && m.owner_saw.as_ref() == Some(&f.revision))
        {
            if f.merging == Merge::Update {
                return at(
                    Stage::Review,
                    Step::UpdateBranch,
                    "Behind its base: updated before judging",
                );
            }
            return work(f, m, now, Kind::Judge);
        }
        // A judge's land verdict cannot replace the person's word for R2.
        if f.classified.class == Class::R2 && m.owner_saw.as_ref() != Some(&f.revision) {
            return at(
                Stage::Owner,
                Step::ListOwner,
                "R2: a person must decide before it lands",
            );
        }
    }
    if !f.authorship_known && f.dependency.is_none() {
        return wait(
            Stage::Review,
            None,
            "Settled; could not read its commits; next pass",
        );
    }
    land(f, now)
}

fn land(f: &Facts, now: u64) -> Decision {
    match f.empty {
        None => {
            return at(
                Stage::Merge,
                Step::Retry {
                    kind: Failure::Empty,
                    at: now.saturating_add(60),
                },
                "What its merge would change is not known",
            );
        }
        Some(true) if f.dependency.is_some() => {
            return wait(Stage::Merge, None, "Already landed; its bot closes it");
        }
        Some(true) => {
            return at(
                Stage::Merge,
                Step::CloseAlreadyLanded,
                "Its base already has everything it changes",
            );
        }
        Some(false) => {}
    }
    if f.merging == Merge::Update {
        return at(
            Stage::Merge,
            Step::UpdateBranch,
            "Settled, but behind its base",
        );
    }
    if f.settings.train {
        return at(
            Stage::Train,
            Step::JoinTrain,
            "Settled; join the merge train",
        );
    }
    if matches!(f.merging, Merge::Unknown | Merge::Refused) {
        return wait(
            Stage::Merge,
            None,
            "GitHub will not yet say it can be merged",
        );
    }
    if f.renovate && f.bot_merges {
        return at(
            Stage::Merge,
            Step::Settle,
            "Dependency update, CI green; its bot merges it",
        );
    }
    at(
        Stage::Merge,
        Step::Merge,
        "Settled; every thread resolved and CI green",
    )
}

fn hand(f: &Facts, m: &mut Memory, now: u64, kind: Kind, delay: bool) -> Decision {
    if let Some(job) = f.jobs.values().find(|j| j.state.live()) {
        // Fix jobs keep the branch across pushes. Other jobs only block the
        // work they were asked for on their own revision.
        if f.jobs
            .iter()
            .any(|(k, j)| k.fixes_branch() && j.id == job.id)
        {
            return wait(Stage::Fix, None, "A fix job still has the branch");
        }
    }
    if delay {
        let quiet = if f.session {
            f.settings.session_handoff_after
        } else {
            f.settings.handoff_after
        };
        let due = f.updated_at.max(f.last_reply_at).saturating_add(quiet);
        if now <= due {
            return wait(
                Stage::Fix,
                Some(due),
                "Findings or CI await handoff after the quiet period",
            );
        }
    }
    work(f, m, now, kind)
}

fn work(f: &Facts, m: &mut Memory, now: u64, kind: Kind) -> Decision {
    let stage = if kind.fixes_branch() {
        Stage::Fix
    } else if kind == Kind::Proof {
        Stage::Proof
    } else {
        Stage::Review
    };
    if let Some(j) = f.jobs.get(&kind).filter(|j| j.revision == f.revision) {
        if j.state.live() {
            return wait(
                stage,
                None,
                "A job is queued or running, or its state is unknown",
            );
        }
        if !m.lost_jobs.insert(j.id.clone()) {
            if j.state == JobState::Done && j.repeated {
                return at(
                    Stage::Owner,
                    Step::ListOwner,
                    "Two jobs ended without their answer",
                );
            }
            if j.state == JobState::Broken {
                let due = if let Some(due) = m.broken_jobs.get(&j.id) {
                    *due
                } else {
                    let n = m.strikes.entry(Failure::Lost).or_default();
                    *n = n.saturating_add(1);
                    let due = now.saturating_add((1u64 << (*n - 1).min(6)).min(60) * 60);
                    m.broken_jobs.insert(j.id.clone(), due);
                    due
                };
                if now < due {
                    return at(
                        stage,
                        Step::Retry {
                            kind: Failure::Lost,
                            at: due,
                        },
                        "The last job broke; trying again later",
                    );
                }
            }
        } else {
            return wait(
                stage,
                Some(now.saturating_add(60)),
                "The job just ended; read its answer once more",
            );
        }
    }
    let step = match kind {
        Kind::Judge => Step::Judge,
        Kind::Proof => Step::Prove,
        _ => Step::HandFix(kind),
    };
    at(stage, step, "Start the next job")
}

fn failure_stage(kind: Failure) -> Stage {
    match kind {
        Failure::Build | Failure::Eval => Stage::Proof,
        Failure::Post => Stage::Review,
        Failure::Job | Failure::Lost | Failure::Answer | Failure::Stop => Stage::Fix,
        _ => Stage::Merge,
    }
}
