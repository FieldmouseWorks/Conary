//! Execute one intention on the freshly read revision.
use super::jobs::{self, Purpose};
use super::persist::{Error, Result};
use super::*;
use conary_core::Store;
use conary_core::plugin::Task;

type Effect = std::result::Result<Vec<Failure>, (Failure, Error)>;
pub(crate) fn act<G: Write>(
    pass: &Pass<'_>,
    github: &G,
    f: &Facts,
    next: &Next,
    record: &mut Record,
    data: &Data,
    mut store: Option<&mut Store>,
) -> Effect {
    let kind = failure(&next.step);
    // Cleanup may have replaced StopJob, or the pause may have arrived
    // during classification. Cancellation still precedes every effect.
    if super::pause::cancel(
        pass.root,
        &f.repo,
        f.number,
        record,
        f.labels.contains(&f.settings.hold_label),
    )
    .map_err(|e| (Failure::Stop, e))?
    {
        data.save(f.number, record)
            .map_err(|e| (Failure::Stop, e))?;
    }
    // A pause written since the decision keeps every effect but the ones
    // that vacate from starting, before any permission is granted.
    if !super::pause::vacates(&next.step) {
        super::pause::effect(pass.root, &f.repo, f.number).map_err(|e| (kind, e))?;
    }
    let repo = f.repo.to_string();
    // Revocation is safety critical; cosmetic label failure must never
    // preserve an earlier success, including while a retry is booked.
    record.live.remove(&f.revision.head);
    record.cleanup.insert(f.revision.head.clone());
    data.save(f.number, record)
        .map_err(|e| (Failure::Status, e))?;
    // While a submitted request is unresolved, waiting withdraws future
    // grants; it does not revoke the authority of that request retroactively.
    let text = if matches!(next.step, Step::Wait { .. })
        && record
            .landing
            .iter()
            .chain(&record.pending)
            .any(|l| l.head == f.revision.head && !l.retry && l.permission.is_some())
    {
        crate::ingest::CLEANUP
    } else {
        &next.description
    };
    github
        .status(&repo, &f.revision.head, false, text)
        .map_err(|e| (Failure::Status, Box::new(e) as Error))?;
    record.cleanup.remove(&f.revision.head);
    data.save(f.number, record)
        .map_err(|e| (Failure::Status, e))?;
    if matches!(next.step, Step::Retry { .. }) {
        return Ok(vec![]);
    }
    let mut prepare = || -> Result<()> {
        let settled = super::pass::grants(next, f);
        let mut scope = || {
            let ledger = now(&mut store, pass.ledger)?;
            if super::pause::vacates(&next.step) && !settled {
                super::scope::cleanup(pass, github, f, record, ledger)
            } else {
                super::scope::current(pass, github, f, record, ledger)
            }
        };
        scope()?;
        github.stage(&repo, f.number, next.stage, &f.settings.stage_labels)?;
        scope()?;
        if settled {
            if next.step == Step::Merge && f.classified.class == conary_core::Class::R2 {
                return Err("R2 requires exact candidate evidence through the train".into());
            }
            // Every grant, including a seated train's Wait, revalidates
            // after preparatory calls and immediately before publication.
            permission(pass, github, f, &next.step, record, &mut store)?;
            record.cleanup.insert(f.revision.head.clone());
            data.save(f.number, record)?;
            github.status(&repo, &f.revision.head, true, &next.description)?;
            permission(pass, github, f, &next.step, record, &mut store)?;
        }
        Ok(())
    };
    if let Err(error) = prepare() {
        let cleanup = super::ledger::cleanup(github, &f.repo, f.number, data, record);
        return Err((Failure::Status, cleanup.err().unwrap_or(error)));
    }
    let mut run = || -> Result<Vec<Failure>> {
        let repo = f.repo.to_string();
        match &next.step {
            Step::Wait { .. } | Step::Retry { .. } => {}

            Step::AskReview(_) => {
                match pass
                    .config
                    .review_ask
                    .as_ref()
                    .filter(|a| a.covers(&f.repo))
                {
                    Some(door) => door.ask(&f.repo, f.number, &f.revision.head)?,
                    None => github.comment(&repo, f.number, super::reviews::ASK)?,
                }
                // Asked only once the door said so, or GitHub took the comment.
                record.memory.asked = Some((f.revision.head.clone(), pass.now));
                if record.memory.first_asked.as_ref().map(|(h, _)| h) != Some(&f.revision.head) {
                    record.memory.first_asked = Some((f.revision.head.clone(), pass.now));
                }
            }
            Step::HandFix(kind) => {
                let (problems, handed) = super::handoff::problems(f, *kind)?.into_iter().unzip();
                pass.start(
                    f,
                    Purpose::Fix(*kind),
                    Task::Fix {
                        repo,
                        pr: f.number,
                        head: f.revision.head.clone(),
                        problems,
                    },
                    handed,
                    record,
                    data,
                )?;
            }
            Step::Stuck(why, detail) => {
                // Saved with the job it starts, in the same write: a wake or
                // a restart after this never hands the same loop or stall
                // again.
                let job = super::pass::job_id(f, record)?;
                record.memory.stuck.push(super::Handoff {
                    why: *why,
                    head: f.revision.head.clone(),
                    stage: (*why == super::Stuck::Stall).then_some(next.stage),
                    at: pass.now,
                    job: job.clone(),
                    answered: false,
                });
                let started = pass.start(
                    f,
                    Purpose::Stuck(*why),
                    Task::Job {
                        repo,
                        workdir: ".".into(),
                        prompt: super::stuck::prompt(f, *why, detail),
                        model: None,
                        effort: None,
                    },
                    Vec::new(),
                    record,
                    data,
                );
                // Not saved with a job, it was never made; saved, its job
                // answers for it, launched or not.
                if started.is_err() && !record.jobs.iter().any(|w| w.job.id == job) {
                    record.memory.stuck.retain(|h| h.job != job);
                }
                started?;
            }
            Step::Concede => super::stuck::concede(github, f, record, data, pass.now)?,
            Step::Judge(decided) => {
                // The pass waits, saying why, on a head with no one author.
                let author = f.author.clone()?;
                pass.start(
                    f,
                    Purpose::Judge,
                    Task::Judge {
                        repo,
                        pr: f.number,
                        head: f.revision.head.clone(),
                        against: format!("{}\n{}", f.title, f.description),
                        author,
                        // The reviewer posts on GitHub; no job of the flow's answered as it.
                        reviewers: Vec::new(),
                        why: super::judging::why(record, &f.revision, decided)?,
                        context: f.context(),
                    },
                    Vec::new(),
                    record,
                    data,
                )?;
            }
            Step::Prepare => {
                let checks = super::proofs::selected(f).into_keys().collect();
                let tree = github.tree(&repo, &f.revision.head)?;
                pass.start(
                    f,
                    Purpose::Prepare,
                    Task::Prepare {
                        repo,
                        head: f.revision.head.clone(),
                        tree,
                        checks,
                    },
                    Vec::new(),
                    record,
                    data,
                )?;
            }
            Step::Prove => {
                let checks = super::proofs::selected(f).into_keys().collect();
                let tree = github.tree(&repo, &f.revision.head)?;
                pass.start(
                    f,
                    Purpose::Proof,
                    Task::Proof {
                        repo,
                        head: f.revision.head.clone(),
                        tree,
                        checks,
                    },
                    Vec::new(),
                    record,
                    data,
                )?;
            }
            Step::PostFix(id) => {
                if super::claims::superseded(f, id) {
                    return Err("a later fix superseded this one's answers".into());
                }
                let at = record
                    .jobs
                    .iter()
                    .position(|w| &w.job.id == id && matches!(w.purpose, Purpose::Fix(_)))
                    .ok_or("fix job missing")?;
                super::publish::answers(github, f, record, data, at, pass.now)?;
                record.jobs[at].posted = true;
            }
            Step::PostProof => {
                return Err("proof output awaits independently attested GitHub evidence".into());
            }
            Step::StopJob(id) => {
                let why = if f.paused.is_some() {
                    super::pause::Stop::Paused
                } else if f.labels.contains(&f.settings.hold_label) {
                    super::pause::Stop::Held
                } else {
                    super::pause::Stop::Obsolete
                };
                let work = record
                    .jobs
                    .iter()
                    .find(|w| &w.job.id == id)
                    .ok_or("job missing")?;
                jobs::cancel(work, why)?;
            }
            Step::StartDeferred(names) => {
                super::deferred::begin(pass, github, f, names, record, data)?
            }
            Step::CancelDeferred(dispatch_id) => {
                super::deferred::cancel(github, f, dispatch_id, pass.now, record, data)?
            }
            Step::UpdateBranch => github.update(&repo, f.number, &f.revision.head)?,
            Step::Recover(recovery) => {
                // Defense in depth: single-attempt-attested repositories are
                // structurally forbidden from issuing partial rerun-failed-jobs.
                if !RerunPolicy::for_evidence(pass.config.evidence.as_ref()).permits(recovery) {
                    return Err(
                        "single-attempt attestation cohesion forbids partial rerun-failed-jobs"
                            .into(),
                    );
                }
                let mut asked = next
                    .recovery
                    .clone()
                    .filter(|r| {
                        &r.recovery == recovery
                            && r.head == f.revision.head
                            && r.answer == Answer::Asked
                    })
                    .ok_or("a recovery decided without its record")?;
                // Saved before GitHub is asked: whatever happens next, no
                // event, wake or restart asks again.
                record.memory.recovered.push(asked.clone());
                data.save(f.number, record)?;
                let asking = match recovery {
                    Recovery::Update => github.update(&repo, f.number, &f.revision.head),
                    Recovery::Rerun(run) => github.rerun(&repo, *run),
                    Recovery::RerunFailedJobs(run) => github.rerun_failed_jobs(&repo, *run),
                    Recovery::Rerequest(suite) => github.rerequest(&repo, *suite),
                };
                asked.answer = match asking {
                    Ok(()) => Answer::Accepted,
                    Err(e) if e.unavailable => Answer::Unanswered(e.to_string()),
                    Err(e) => {
                        Answer::Refused(e.said().map_or_else(|| e.to_string(), str::to_owned))
                    }
                };
                let accepted = asked.answer == Answer::Accepted;
                let text = asked.describe();
                *record.memory.recovered.last_mut().expect("pushed above") = asked;
                data.save(f.number, record)?;
                // Nothing on GitHub changes to bring a refusal back: its
                // status says it now.
                if !accepted {
                    github.status(&repo, &f.revision.head, false, &text)?;
                    return Ok(vec![Failure::Status]);
                }
            }
            Step::Merge => {
                // The commits its merge squashes, whose trailers its landing
                // commit says, read before the hold: the merge pins them by
                // the head. One whose trailers can't all be read isn't merged
                // without them.
                let base = f.base_tip.as_deref().unwrap_or(&f.revision.base);
                let commits = github.compare_commits(&repo, base, &f.revision.head)?;
                let message = crate::train::Message::new(
                    &f.title,
                    f.number,
                    crate::train::messages(&commits)?.iter().map(String::as_str),
                );
                let ledger = store
                    .as_deref_mut()
                    .ok_or("landing needs an anchored ledger")?;
                // The hold comes before the last look, at GitHub and at the
                // ledger: waiting for it, however long another writer holds
                // it, is over before anything the merge rests on is read.
                let merged = ledger.holding(|ledger| -> Result<_> {
                    if !super::ledger::unchanged(f, ledger) {
                        return Ok(None);
                    }
                    // Read back the permission; a refused or lost status write must never merge.
                    let statuses = github.statuses(&repo, &f.revision.head)?;
                    let status = statuses
                        .iter()
                        .find(|s| s.context == SETTLED)
                        .filter(|s| {
                            s.state == "success"
                                && s.creator
                                    .as_ref()
                                    .is_some_and(|u| f.settings.flow_authors.contains(&u.login))
                        })
                        .ok_or("review-settled permission was not observed")?;
                    // The last look before the merge's intent is saved.
                    permission(pass, github, f, &next.step, record, &mut Some(&mut *ledger))?;
                    super::ledger::begin(f, record)?;
                    record.landing.as_mut().expect("saved intent").permission =
                        Some(crate::ingest::Permission {
                            login: status
                                .creator
                                .as_ref()
                                .expect("authenticated above")
                                .login
                                .clone(),
                            at: status
                                .created_at
                                .clone()
                                .ok_or("permission timestamp unknown")?,
                            text: status.description.clone(),
                        });
                    data.save(f.number, record)?;
                    Ok(Some(github.land(
                        &repo,
                        f.number,
                        &f.revision.head,
                        &message,
                    )?))
                })??;
                let Some(merged) = merged else {
                    github.status(&repo, &f.revision.head, false, WITHDRAWN)?;
                    return Err("another writer recorded about the PR before its merge".into());
                };
                match merged {
                    crate::train::PrMerge::Merged => {
                        record.landing.as_mut().expect("saved intent").accepted = true;
                        data.save(f.number, record)?;
                    }
                    crate::train::PrMerge::Refused(why) => {
                        record.landing.as_mut().expect("saved intent").permission = None;
                        data.save(f.number, record)?;
                        return Err(why.into());
                    }
                }
                super::ledger::reconcile(
                    pass,
                    github,
                    &f.repo,
                    data,
                    store
                        .as_deref_mut()
                        .ok_or("landing needs an anchored ledger")?,
                    f.number,
                    record,
                )?;
            }
            Step::JoinTrain => {
                let store = store
                    .as_deref_mut()
                    .ok_or("train needs an anchored ledger")?;
                let config = super::ledger::config(&f.settings);
                // Held from before the last look, as for a merge.
                let joined = store.holding(|store| -> Result<_> {
                    permission(pass, github, f, &next.step, record, &mut Some(&mut *store))?;
                    Ok(crate::train::join(
                        store,
                        github,
                        github,
                        &config,
                        &f.repo,
                        f.number,
                        &f.revision.head,
                        &record.memory.raised.iter().cloned().collect::<Vec<_>>(),
                        pass.policy,
                        Some(
                            &pass
                                .config
                                .evidence
                                .as_ref()
                                .ok_or("producer verifier missing")?
                                .negatives()?,
                        ),
                    )?)
                })??;
                if !matches!(
                    joined,
                    crate::train::Joined::Joined | crate::train::Joined::Already
                ) {
                    return Err(format!("train did not admit PR: {joined:?}").into());
                }
            }
            Step::LeaveTrain => {
                crate::train::leave(
                    store
                        .as_deref_mut()
                        .ok_or("train needs an anchored ledger")?,
                    github,
                    github,
                    &f.repo,
                    f.number,
                    conary_core::op::Left::Unready,
                )?;
            }
            Step::DisableAutoMerge => github.disable_auto_merge(&repo, f.number)?,
            Step::CloseAlreadyLanded => github.close(&repo, f.number)?,
            // A request is listed once: a later wait that joins it adds
            // no listing, even where the label has since come off.
            Step::ListOwner(door) => {
                let label = f
                    .settings
                    .owner_label
                    .as_deref()
                    .ok_or("no owner label is set")?;
                let open = record.memory.owner.iter_mut().find(|r| {
                    r.key.door == *door
                        && r.key.pr == f.number
                        && r.key.scope == f.revision.description
                });
                // The decision opened it, so its absence is a bug to
                // surface, never a listing to skip or repeat.
                let request = open.ok_or("no owner request is open for this listing")?;
                if !request.listed {
                    github.owner(&repo, f.number, label)?;
                    request.listed = true;
                    if !f.labels.iter().any(|l| l == label) {
                        record.memory.labelled = true;
                    }
                }
            }
        }
        Ok(vec![kind, Failure::Status])
    };
    let result = run();
    if result.is_err() {
        if let Err(error) = super::ledger::cleanup(github, &f.repo, f.number, data, record) {
            return Err((Failure::Status, error));
        }
    } else {
        record.cleanup.remove(&f.revision.head);
        if next.step != Step::Merge && super::pass::grants(next, f) {
            record.live.insert(f.revision.head.clone());
        }
    }
    result.map_err(|error| (kind, error))
}
fn failure(step: &Step) -> Failure {
    match step {
        Step::UpdateBranch | Step::Recover(Recovery::Update) => Failure::Update,
        Step::Recover(_) => Failure::Rerun,
        Step::Merge | Step::JoinTrain | Step::LeaveTrain => Failure::Merge,
        Step::CloseAlreadyLanded => Failure::Close,
        Step::AskReview(_) => Failure::Post,
        Step::PostFix(_) | Step::Concede => Failure::Answer,
        Step::Prepare | Step::Prove | Step::PostProof => Failure::Build,
        Step::HandFix(_) | Step::Judge(_) | Step::Stuck(..) => Failure::Job,
        Step::StopJob(_) => Failure::Stop,
        Step::StartDeferred(_) | Step::CancelDeferred(_) => Failure::Deferred,
        _ => Failure::Status,
    }
}

/// The ledger as it stands now, for a permission's look: the store caught up
/// on what other writers recorded since its last write, or the one `read`.
fn now<'a>(
    store: &'a mut Option<&mut Store>,
    read: Option<&'a conary_core::Ledger>,
) -> Result<Option<&'a conary_core::Ledger>> {
    match store {
        Some(store) => Ok(Some(store.refresh()?)),
        None => Ok(read),
    }
}

/// What a withdrawn permission's status says.
const WITHDRAWN: &str = "Permission inputs changed; read the PR again";

/// Every look at what `step` is permitted on, which withdraws the permission
/// if one fails. No wait for the ledger's lock may come between these looks
/// and the effect they permit: an effect that needs the lock takes its hold
/// first and looks under it, and here the last wait for it comes before the
/// evidence, the scope, the approval and the pause are last read.
pub(super) fn permission<G: Write>(
    pass: &Pass<'_>,
    github: &G,
    f: &Facts,
    step: &Step,
    record: &Record,
    store: &mut Option<&mut Store>,
) -> Result<()> {
    let mut look = || -> Result<()> {
        super::scope::current(pass, github, f, record, now(store, pass.ledger)?)?;
        let ledger = now(store, pass.ledger)?;
        // Read again only to act on: what it can't tell was recorded
        // when the decision was made, and fails the effect here.
        super::evidence::authorize(pass, github, f, step, &mut Default::default())?;
        super::scope::current(pass, github, f, record, ledger)?;
        if super::approval::required(f) && !super::approval::approved(pass, github, f)? {
            return Err("An authorized person's scoped approval was withdrawn".into());
        }
        // Read last: the scope, evidence and approval reads above can all
        // race a pause. This also validates a status just published.
        super::pause::effect(pass.root, &f.repo, f.number)
    };
    let result = look();
    if result.is_err() {
        github.status(&f.repo.to_string(), &f.revision.head, false, WITHDRAWN)?;
    }
    result
}
