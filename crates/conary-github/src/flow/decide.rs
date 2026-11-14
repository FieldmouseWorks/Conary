use super::*;
use conary_core::Class;

pub(super) type Decision = (Stage, Step, String);
pub(super) fn at(stage: Stage, step: Step, description: &str) -> Decision {
    (stage, step, description.into())
}
pub(super) fn wait(stage: Stage, until: Option<u64>, why: &str) -> Decision {
    at(stage, Step::Wait { until }, why)
}

pub(super) fn decide(f: &Facts, m: &mut Memory, now: u64) -> Decision {
    if let Some(kind) = f.failed {
        return failed(m, kind, now, "The step failed; trying again later");
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
    if let Some(id) = m.stopping.first() {
        return at(
            Stage::Fix,
            Step::StopJob(id.clone()),
            "Stop obsolete work before starting another job",
        );
    }
    if let Some(job) = super::proofs::obsolete_inputs(f) {
        return at(
            Stage::ProofSlot,
            Step::StopJob(job.id.clone()),
            "Stop inputs for an obsolete proof",
        );
    }
    if let Some(job) = super::proofs::obsolete(f) {
        return at(
            Stage::Proof,
            Step::StopJob(job.id.clone()),
            "Stop an obsolete proof before starting more work",
        );
    }
    // Only proofs and judgments belong to a head. Fix jobs own the branch
    // across pushes, including their own push, and must be left running.
    if let Some((_, job)) = f.jobs.iter().find(|(kind, job)| {
        !kind.fixes_branch()
            && job.state.live()
            && job.revision.head != f.revision.head
            && !(**kind == Kind::Proof && super::proofs::useful(f, &job.id))
    }) {
        return at(
            Stage::Fix,
            Step::StopJob(job.id.clone()),
            "Stop work answering for an earlier head",
        );
    }
    if let Some(decision) = super::claims::look(f, m, now) {
        return decision;
    }
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
    if f.labels.contains(&f.settings.hold_label) {
        return wait(Stage::None, None, "On hold (the hold label)");
    }
    if f.stacked_on.is_some() {
        return wait(
            Stage::Base,
            None,
            "Stacked on an open PR: waiting to be retargeted",
        );
    }
    if f.labels.contains(&f.settings.owner_label) && !f.owner_approved {
        return at(
            Stage::Owner,
            Step::ListOwner,
            "The owner decides (the needs-peter label)",
        );
    }
    if let Some((kind, job)) = f
        .jobs
        .iter()
        .find(|(kind, job)| kind.fixes_branch() && job.state.live())
    {
        return wait(
            Stage::Fix,
            Some(job_wake(f, *kind, job, now)),
            "A fix job still owns the branch",
        );
    }
    if f.major_generation.is_some() && f.major.is_none() {
        return wait(
            Stage::Review,
            None,
            "Sweep decision rejected; a valid newer issuance is required",
        );
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
    if super::proofs::posting(f).is_some() {
        return at(
            Stage::Proof,
            Step::PostProof,
            "Posting the remaining machine proof verdicts",
        );
    }
    let ci = ci(f, m, now);
    // Unknown provenance is someone else's code until read: only a history
    // known to be the bots' alone skips the review loop.
    let edited = f.bot_edited != Some(false) || f.gate_edited;
    if f.dependency.is_some() {
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
    }
    match super::review::look(f, m, now, f.dependency.is_none() || edited) {
        super::review::Look::Next(decision) => decision,
        super::review::Look::Settled { capped } => after_review(f, m, now, ci, capped),
    }
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
    if by_train(f) || !f.settings.require_ci {
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
    // A partial failure may already make CI red. The remaining stored
    // verdicts must be published before handing that failure to a fix.
    if f.proof == Proof::Unposted {
        return at(
            Stage::Proof,
            Step::PostProof,
            "Posting machine proof verdicts",
        );
    }
    if ci == Ci::Red || f.proof == Proof::Failed {
        return hand(f, m, now, Kind::Red, true);
    }
    let verdict = judgment(f);
    if verdict == Some(Judgment::Misdescribed) {
        return hand(f, m, now, Kind::Description, false);
    }
    if verdict == Some(Judgment::Fail) {
        return hand(f, m, now, Kind::JudgeFailure, false);
    }
    if verdict == Some(Judgment::Owner) && !f.owner_approved {
        return at(
            Stage::Owner,
            Step::ListOwner,
            "The judge sent it to the owner",
        );
    }
    let pending: Vec<_> = f.checks.iter().filter(|c| c.state == Ci::Pending).collect();
    let machine = f.settings.machine();
    let proof_only = !pending.is_empty()
        && pending
            .iter()
            .all(|c| machine.contains(&c.name.to_lowercase()));
    let early = pending.iter().any(|c| {
        matches!(
            f.settings.recipe(&c.name),
            Some(Recipe::Build(_) | Recipe::Run(_))
        )
    });
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
        Proof::Needed if ci != Ci::Pending || proof_only || early => {
            let retry = super::proofs::retry(f, m, now);
            let plan = super::proofs::next(f, now);
            if let Some(decision) = retry {
                if let Step::Retry {
                    kind: Failure::Lost,
                    at,
                } = decision.1
                    && let Some((Stage::ProofSlot, mut step, description, _)) = plan
                {
                    if matches!(step, Step::Wait { .. }) {
                        step = Step::Wait { until: Some(at) };
                    }
                    return (Stage::ProofSlot, step, description);
                }
                return decision;
            }
            if let Some((stage, step, description, _)) = plan {
                return (stage, step, description);
            }
            return work(f, m, now, Kind::Proof);
        }
        _ => {}
    }
    if ci == Ci::Pending {
        return wait(
            Stage::Ci,
            m.no_checks_since.map(|t| t.saturating_add(300)),
            &format!(
                "Settled; waiting for CI: {}",
                pending
                    .iter()
                    .map(|c| c.name.as_str())
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    }
    let why = judge_why(f, ci, capped);
    let requires_judge = !why.is_empty()
        || f.jobs.get(&Kind::Judge).is_some_and(|j| {
            j.revision.head == f.revision.head && j.revision.base == f.revision.base
        });
    if requires_judge {
        if !matches!(verdict, Some(Judgment::Pass))
            && !(verdict == Some(Judgment::Owner) && f.owner_approved)
        {
            if f.merging == Merge::Update {
                return at(
                    Stage::Review,
                    Step::UpdateBranch,
                    "Behind its base: updated before judging",
                );
            }
            let mut decision = work(f, m, now, Kind::Judge);
            if let Step::Judge(reasons) = &mut decision.1 {
                *reasons = why;
            }
            return decision;
        }
        // A judge's land verdict cannot replace the person's word for R2.
        if f.classified.class == Class::R2 && !f.owner_approved {
            return at(
                Stage::Owner,
                Step::ListOwner,
                "R2: a person must decide before it lands",
            );
        }
    }
    if !f.authorship_known {
        return wait(
            Stage::Review,
            None,
            "Settled; could not read its commits; next pass",
        );
    }
    land(f, m, now)
}

/// Every reason the flow has to ask a judge now, in the plugin contract's
/// words: the one place that list is made.
pub(super) fn judge_why(f: &Facts, ci: Ci, capped: bool) -> Vec<Trigger> {
    [
        (f.classified.class == Class::R2, Trigger::R2),
        (f.needs_judge, Trigger::AuthorEngine),
        (f.unseen, Trigger::HiddenFiles),
        (ci == Ci::None, Trigger::NoCi),
        (f.judge_asked, Trigger::Asked),
        (capped, Trigger::RoundLimit),
        (
            f.dependency == Some(Dependency::Major) && !f.renovate,
            Trigger::MajorDependency,
        ),
    ]
    .into_iter()
    .filter_map(|(holds, trigger)| holds.then_some(trigger))
    .collect()
}

fn land(f: &Facts, m: &mut Memory, now: u64) -> Decision {
    match f.empty {
        None => {
            return failed(
                m,
                Failure::Empty,
                now,
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
    if by_train(f) {
        return at(
            Stage::Train,
            Step::JoinTrain,
            "Settled; join the merge train",
        );
    }
    if matches!(f.merging, Merge::Unknown | Merge::Refused) {
        return failed(
            m,
            Failure::Mergeable,
            now,
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

pub(super) fn hand(f: &Facts, m: &mut Memory, now: u64, kind: Kind, delay: bool) -> Decision {
    if f.renovate && f.dependency.is_some() {
        let red = f.checks.iter().any(|c| c.state == Ci::Red);
        let until = f
            .rebase_removed
            .iter()
            .copied()
            .chain(f.timeline_cut_at)
            .map(|t| t.saturating_add(f.settings.renovate_run))
            .filter(|t| now < *t)
            .max();
        if red || f.rebase_requested || until.is_some() {
            return wait(
                Stage::Fix,
                until,
                "Dependency sweep or a Renovate rebase still owns the branch",
            );
        }
    }
    if let Some((_, job)) = f
        .jobs
        .iter()
        .find(|(kind, job)| kind.fixes_branch() && job.state.live())
    {
        return wait(
            Stage::Fix,
            Some(job_wake(f, Kind::Findings, job, now)),
            "A fix job still has the branch",
        );
    }
    if delay {
        let quiet = if f.session {
            f.settings.session_handoff_after
        } else {
            f.settings.handoff_after
        };
        let due = f
            .updated_at
            .max(f.last_reply_at)
            .max(m.seen)
            .saturating_add(quiet);
        if now <= due {
            return wait(
                Stage::Fix,
                Some(due.saturating_add(1)),
                "Findings or CI await handoff after the quiet period",
            );
        }
    }
    work(f, m, now, kind)
}

pub(super) fn work(f: &Facts, m: &mut Memory, now: u64, kind: Kind) -> Decision {
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
                Some(job_wake(f, kind, j, now)),
                "A job is queued or running, or its state is unknown",
            );
        }
        if !m.lost_jobs.insert(j.id.clone()) {
            let second = if j.state == JobState::Done {
                m.strikes.remove(&Failure::Lost);
                let ended = m.unanswered.entry((kind, j.revision.clone())).or_default();
                ended.insert(j.id.clone());
                ended.len() >= 2 || j.repeated
            } else {
                false
            };
            if second {
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
        Kind::Judge => Step::Judge(Vec::new()),
        Kind::Proof => Step::Prove,
        _ => Step::HandFix(kind),
    };
    at(stage, step, "Start the next job")
}

fn job_wake(f: &Facts, kind: Kind, job: &Job, now: u64) -> u64 {
    let unit = kind == Kind::Proof
        && f.checks.iter().filter(|c| c.state == Ci::Pending).any(|c| {
            matches!(
                f.settings.recipe(&c.name),
                Some(Recipe::Build(_) | Recipe::Run(_))
            )
        });
    let timeout = if unit {
        f.settings.proof_timeout
    } else {
        f.settings.job_timeout
    };
    let deadline = job.started.saturating_add(timeout).saturating_add(300);
    if now < deadline {
        deadline
    } else {
        now.saturating_add(900)
    }
}

fn failed(m: &mut Memory, kind: Failure, now: u64, why: &str) -> Decision {
    let times = m.strikes.entry(kind).or_default();
    *times = times.saturating_add(1);
    let due = now.saturating_add((1u64 << (*times - 1).min(6)).min(60) * 60);
    m.retry = Some((kind, due));
    at(failure_stage(kind), Step::Retry { kind, at: due }, why)
}

fn failure_stage(kind: Failure) -> Stage {
    match kind {
        Failure::Build | Failure::Eval => Stage::Proof,
        Failure::Post => Stage::Review,
        Failure::Job | Failure::Lost | Failure::Answer | Failure::Stop => Stage::Fix,
        _ => Stage::Merge,
    }
}

fn by_train(f: &Facts) -> bool {
    f.settings.train && f.revision.base == f.default_branch
}
