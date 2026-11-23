use super::*;
use conary_core::Class;

pub(super) type Decision = (Stage, Step, String);
pub(super) fn at(stage: Stage, step: Step, description: &str) -> Decision {
    (stage, step, description.into())
}
pub(super) fn wait(stage: Stage, until: Option<u64>, why: &str) -> Decision {
    at(stage, Step::Wait { until }, why)
}

/// Hand the PR to its owner for a one-way door, or wait unlisted when no
/// owner label is set. Only a door lists the owner: every other call is
/// the flow's own, and its status shows it.
pub(super) fn owner(f: &Facts, door: Door) -> Decision {
    if f.settings.owner_label.is_some() {
        at(Stage::Owner, Step::ListOwner(door), &door.to_string())
    } else {
        wait(
            Stage::Owner,
            None,
            &format!("{door}; no owner label is set to list it"),
        )
    }
}

/// Two jobs on one revision ended without their answer: no third is
/// started, and the wait is overdue at once, for its recovery and then the
/// flow's status ([`super::state`]), never the owner.
pub(super) const LOST: &str = "Two jobs ended without their answer: stalled, no third is started";

/// A held major waits for the sweep's word. The sweep names no door, so
/// it's never the owner's to hear of.
const HELD: &str = "Major update held by the dependency sweep, which names no door";

pub(super) fn decide(f: &Facts, m: &mut Memory, now: u64) -> Decision {
    if let Some(decision) = vacate(f, m) {
        return decision;
    }
    if let Some(kind) = f.failed {
        let decision = failed(
            m,
            kind,
            now,
            &said("The step failed; trying again later", &f.failed_why),
        );
        m.retry_why = f.failed_why.clone();
        return decision;
    }
    if let Some((kind, due)) = m.retry {
        if now < due {
            return at(
                failure_stage(kind),
                Step::Retry { kind, at: due },
                &said("Waiting to try the failed step again", &m.retry_why),
            );
        }
        m.retry = None;
        m.retry_why = None;
    }
    if let Some(dispatch_id) = f.deferred_stale.first() {
        return at(
            Stage::Ci,
            Step::CancelDeferred(dispatch_id.clone()),
            "Cancel a deferred check that is obsolete or lost",
        );
    }
    if let Some(id) = m.stopping.first() {
        return at(
            Stage::Fix,
            Step::StopJob(id.clone()),
            "Stop obsolete work before starting another job",
        );
    }
    if let Some(job) = super::proofs::obsolete_preparation(f) {
        return at(
            Stage::Proof,
            Step::StopJob(job.id.clone()),
            "Stop preparing inputs for an obsolete proof",
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
        !kind.fixes_branch() && job.state.live() && job.revision.head != f.revision.head
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
        return owner(f, Door::Outside);
    }
    // The snapshot's degraded read is recorded and named; its wait is the
    // flow's, overdue in its status like any other.
    if !f.complete {
        return wait(
            Stage::None,
            None,
            "Too much to read: waiting for a snapshot GitHub gives in full",
        );
    }
    if f.labels.contains(&f.settings.hold_label) {
        return wait(Stage::None, None, "On hold (the hold label)");
    }
    if f.base_held {
        return wait(Stage::Base, None, "Waiting for the default branch's CI");
    }
    if f.stacked_on.is_some() {
        return wait(
            Stage::Base,
            None,
            "Stacked on an open PR: waiting to be retargeted",
        );
    }
    match super::approval::person_label(f, m) {
        _ if f.owner_approved => {}
        Some(super::approval::PersonLabel::Person) => return owner(f, Door::Label),
        // A person's door can't be told from an App's listing: the flow
        // neither lists it nor goes on past it, and reads it again.
        Some(super::approval::PersonLabel::Untold) => {
            return wait(
                Stage::None,
                None,
                "The owner label is on, and who put it on can't be told",
            );
        }
        None => {}
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
            return wait(Stage::Review, None, HELD);
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
    // Owned deferred failures need repair even without a configured reviewer.
    if !f.deferred_failures.is_empty() {
        return hand(f, m, now, Kind::Red, true);
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
            return wait(Stage::Review, None, HELD);
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
        super::review::Look::Next(decision) => {
            super::deferred::without_reviewer(f, ci == Ci::Red, &decision).unwrap_or(decision)
        }
        super::review::Look::Settled { capped, changed } => {
            after_review(f, m, now, ci, capped, changed)
        }
    }
}

/// A paused or held PR keeps nothing running. Every worker of its that
/// hasn't answered is stopped at once, through the stop path, so a proof's
/// slot frees as soon as its worker ends, and so is every deferred run it
/// owns; nothing new starts; and what its workers finished stays, so nothing is redone once it's lifted. A pause
/// then waits here, before anything else is decided. A hold waits where it
/// always has, after what an owner must hear of first.
fn vacate(f: &Facts, m: &mut Memory) -> Option<Decision> {
    let held = f.labels.contains(&f.settings.hold_label);
    if f.paused.is_none() && !held {
        return None;
    }
    if let Some(first) = f.working.first() {
        m.stopping.extend(f.working.iter().cloned());
        let why = match &f.paused {
            Some(pause) => format!("Paused ({}): stopping its work", pause.reason),
            None => "On hold (the hold label): stopping its work".into(),
        };
        return Some(at(Stage::None, Step::StopJob(first.clone()), &why));
    }
    // Owned deferred runs stop too: whatever was already due to stop first,
    // then what is still running for this head.
    if let Some(dispatch_id) = f.deferred_stale.first().or(f.deferred_running.first()) {
        return Some(at(
            Stage::None,
            Step::CancelDeferred(dispatch_id.clone()),
            "Paused or on hold: stopping its deferred checks",
        ));
    }
    f.paused
        .as_ref()
        .map(|pause| wait(Stage::None, None, &format!("Paused: {}", pause.reason)))
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

/// The judgment that speaks for this revision now. A fail or misdescribed
/// verdict always does. A pass or owner verdict only while it covers every
/// reason in `why` and the discussion now ([`Judged::covers`]); otherwise
/// the flow asks again before granting anything, and the old issuance stays.
fn judgment(f: &Facts, why: &[Trigger]) -> Option<Judgment> {
    f.judgment
        .as_ref()
        .filter(|j| j.revision == f.revision)
        .filter(|j| {
            !matches!(j.verdict, Judgment::Pass | Judgment::Owner) || j.covers(why, &f.context())
        })
        .map(|j| j.verdict)
}

fn after_review(
    f: &Facts,
    m: &mut Memory,
    now: u64,
    ci: Ci,
    capped: bool,
    changed: bool,
) -> Decision {
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
    let why = judge_why(f, ci, capped, changed);
    let verdict = judgment(f, &why);
    if verdict == Some(Judgment::Misdescribed) {
        return hand(f, m, now, Kind::Description, false);
    }
    if verdict == Some(Judgment::Fail) {
        return hand(f, m, now, Kind::JudgeFailure, false);
    }
    if verdict == Some(Judgment::Owner) && !f.owner_approved {
        return owner(f, Door::Judge);
    }
    // Review is ready here: the deferred checks start, and cheap checks have
    // been running all along. A judge's pending verdict on exhausted rounds,
    // or on a PR changed after its clean review, is not readiness.
    let ready = !(capped || changed)
        || verdict == Some(Judgment::Pass)
        || verdict == Some(Judgment::Owner) && f.owner_approved;
    if ready && let Some(decision) = super::deferred::start(f) {
        return decision;
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
            let decision = proof(f, m, now);
            // Zero slots run no proof: nothing starts, not even a
            // preparation, until a machine sets some.
            if matches!(decision.1, Step::Prove | Step::Prepare)
                && f.settings.proof_slots == Some(0)
            {
                return wait(
                    Stage::ProofSlot,
                    None,
                    "No proof slots are set: no machine proof starts",
                );
            }
            return decision;
        }
        _ => {}
    }
    if ci == Ci::Pending {
        // A wait on a check books no wake: its events end it. Only a PR no
        // check has reported for wakes, to tell when none ever will.
        return wait(
            Stage::Ci,
            m.no_checks_since
                .filter(|_| f.checks.is_empty())
                .map(|t| t.saturating_add(300)),
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
    let requires_judge = !why.is_empty()
        || f.jobs.get(&Kind::Judge).is_some_and(|j| {
            j.revision.head == f.revision.head && j.revision.base == f.revision.base
        });
    if requires_judge
        && !matches!(verdict, Some(Judgment::Pass))
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
    // After the judgment, which readiness may need before any evidence exists.
    if let Some(decision) = super::deferred::gate(f) {
        return decision;
    }
    if requires_judge {
        // A judge's land verdict cannot replace the person's word for R2.
        if f.classified.class == Class::R2 && !f.owner_approved {
            return owner(f, Door::R2);
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
pub(super) fn judge_why(f: &Facts, ci: Ci, capped: bool, changed: bool) -> Vec<Trigger> {
    [
        (f.classified.class == Class::R2, Trigger::R2),
        (f.needs_judge, Trigger::AuthorEngine),
        (ci == Ci::None, Trigger::NoCi),
        (f.judge_asked, Trigger::Asked),
        (capped, Trigger::RoundLimit),
        (changed, Trigger::Changed),
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
        // GitHub's test merge goes stale each time trunk moves, and doesn't
        // come back until the head does. The train's candidate merges the
        // head again anyway and takes an empty merge out (`train::step`), so
        // a train PR joins, or keeps its seat, without GitHub's preview.
        None if by_train(f) => {}
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
    at(
        Stage::Merge,
        Step::Merge,
        "Settled; every thread resolved and CI green",
    )
}

pub(super) fn hand(f: &Facts, m: &mut Memory, now: u64, kind: Kind, delay: bool) -> Decision {
    gate(f, m, now, delay).unwrap_or_else(|| work(f, m, now, kind))
}

/// Hand the PR to the stuck role for `why` ([`super::stuck`]), once the
/// branch is free and the PR as quiet as a fix's handoff waits for.
pub(super) fn stuck(
    f: &Facts,
    m: &mut Memory,
    now: u64,
    why: super::Stuck,
    detail: String,
) -> Decision {
    gate(f, m, now, true).unwrap_or_else(|| {
        let description = format!("Handing it to the stuck role: {detail}");
        (Stage::Fix, Step::Stuck(why, detail), description)
    })
}

/// What a handoff waits on before it's made: the dependency sweep or
/// Renovate, a job that owns the branch, or, with `delay`, the quiet period.
pub(super) fn gate(f: &Facts, m: &Memory, now: u64, delay: bool) -> Option<Decision> {
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
            return Some(wait(
                Stage::Fix,
                until,
                "Dependency sweep or a Renovate rebase still owns the branch",
            ));
        }
    }
    if let Some((_, job)) = f
        .jobs
        .iter()
        .find(|(kind, job)| kind.fixes_branch() && job.state.live())
    {
        return Some(wait(
            Stage::Fix,
            Some(job_wake(f, Kind::Findings, job, now)),
            "A fix job still has the branch",
        ));
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
            return Some(wait(
                Stage::Fix,
                Some(due.saturating_add(1)),
                "Findings or CI await handoff after the quiet period",
            ));
        }
    }
    None
}

fn proof(f: &Facts, m: &mut Memory, now: u64) -> Decision {
    // A check stalled on its deadlines starts nothing, and is retried for
    // nothing: decided before any retry is counted.
    if let Some(stall) = super::proofs::stalled(f) {
        return wait(Stage::Proof, None, &stall.to_string());
    }
    let plan = super::proofs::next(f, now);
    // With no program to run, only model-run checks are left: they wait,
    // with their reason, and no earlier proof is retried for them.
    let retry = if super::proofs::selected(f).is_empty() {
        None
    } else {
        super::proofs::retry(f, m, now)
    };
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
    match plan {
        Some((stage, step, description, _)) => (stage, step, description),
        None => wait(Stage::Proof, None, "No machine proof is pending"),
    }
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
        // A judge's answer that came and no longer speaks for the revision
        // is superseded, never missing: ask again at once.
        if j.state != JobState::Answered {
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
                    return wait(stage, None, LOST);
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
    }
    let step = match kind {
        Kind::Judge => Step::Judge(Vec::new()),
        Kind::Proof => Step::Prove,
        _ => Step::HandFix(kind),
    };
    at(stage, step, "Start the next job")
}

fn job_wake(f: &Facts, kind: Kind, job: &Job, now: u64) -> u64 {
    // A proof is a program: a model-run check is never one. Its deadline
    // is the one it started with.
    let timeout = if kind == Kind::Proof {
        f.proof_data
            .records
            .iter()
            .find(|r| r.job.id == job.id)
            .map_or(f.settings.proof_timeout, |r| r.sized.seconds)
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
    m.retry_why = None;
    at(failure_stage(kind), Step::Retry { kind, at: due }, why)
}

/// A failed step's status, and what its effect said when it said anything.
fn said(status: &str, why: &Option<String>) -> String {
    match why {
        Some(why) => format!("{status}: {why}"),
        None => status.to_owned(),
    }
}

fn failure_stage(kind: Failure) -> Stage {
    match kind {
        Failure::Build | Failure::Eval => Stage::Proof,
        Failure::Post => Stage::Review,
        Failure::Deferred => Stage::Ci,
        Failure::Job | Failure::Lost | Failure::Answer | Failure::Stop => Stage::Fix,
        _ => Stage::Merge,
    }
}

pub(super) fn by_train(f: &Facts) -> bool {
    f.settings.train && f.revision.base == f.default_branch
}
