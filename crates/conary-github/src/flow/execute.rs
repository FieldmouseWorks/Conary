//! Execute one intention on the freshly read revision.
use super::jobs::{self, Purpose};
use super::persist::{Error, Result};
use super::*;
use conary_core::Store;
use conary_core::plugin::{self, Task};

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
        super::scope::current(pass, github, f, record)?;
        github.stage(&repo, f.number, next.stage)?;
        super::scope::current(pass, github, f, record)?;
        let settled = super::pass::grants(next, f);
        if settled {
            if matches!(next.step, Step::Merge | Step::Settle)
                && f.classified.class == conary_core::Class::R2
            {
                return Err("R2 requires exact candidate evidence through the train".into());
            }
            super::evidence::authorize(pass, github, f)?;
            super::scope::current(pass, github, f, record)?;
            record.cleanup.insert(f.revision.head.clone());
            if next.step == Step::Settle {
                record.landing = Some(super::ledger::intent(f, true)?);
            }
            data.save(f.number, record)?;
            github.status(&repo, &f.revision.head, true, &next.description)?;
            permission(pass, github, f, record)?;
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
            Step::Settle => permission(pass, github, f, record)?,

            Step::QueueReview(role) => {
                let author = author(github, f, record)?;
                pass.start(
                    f,
                    Purpose::Review(role.clone()),
                    Task::Review {
                        repo,
                        pr: f.number,
                        base: f.base_tip.clone().ok_or("base tip unknown")?,
                        head: f.revision.head.clone(),
                        author,
                    },
                    record,
                    data,
                )?;
            }
            Step::HandFix(kind) => {
                let problems = super::handoff::problems(github, f, &record.memory, *kind)?;
                pass.start(
                    f,
                    Purpose::Fix(*kind),
                    Task::Fix {
                        repo,
                        head: f.revision.head.clone(),
                        problems,
                    },
                    record,
                    data,
                )?;
            }
            Step::Judge => {
                let author = author(github, f, record)?;
                pass.start(
                    f,
                    Purpose::Judge,
                    Task::Judge {
                        repo,
                        head: f.revision.head.clone(),
                        against: format!("{}\n{}", f.title, f.description),
                        author,
                    },
                    record,
                    data,
                )?;
            }
            Step::Prove => {
                let checks = super::pass::recipes(f).into_keys().collect();
                let tree = github.tree(&repo, &f.revision.head)?;
                pass.start(
                    f,
                    Purpose::Proof,
                    Task::Proof { repo, tree, checks },
                    record,
                    data,
                )?;
            }
            Step::PostReview(role) => {
                let work = record
                    .jobs
                    .iter_mut()
                    .rev()
                    .find(|w| matches!(&w.purpose, Purpose::Review(r) if r == role) && !w.posted)
                    .ok_or("review answer missing")?;
                let completion = jobs::completion(work, pass.now)?;
                let Some(Ok(plugin::Response {
                    answer: plugin::Answer::Review { findings },
                    ..
                })) = completion.result
                else {
                    return Err("review answer invalid".into());
                };
                if work.job.revision != f.revision {
                    return Err("review scope moved".into());
                }
                let body =
                    super::write::review_body(role, &work.job.revision, completion.at, &findings);
                // A write can succeed even when its response is lost. Reconcile
                // the exact authenticated marker before spending another round.
                let already_posted = github.reviews(&repo, f.number)?.iter().any(|r| {
                    r.commit_id.as_deref() == Some(work.job.revision.head.as_str())
                        && r.body.as_deref() == Some(body.as_str())
                        && r.user
                            .as_ref()
                            .is_some_and(|u| f.settings.review_authors.contains(&u.login))
                });
                if !already_posted {
                    github.review(&repo, f.number, &work.job.revision.head, &body)?;
                }
                work.posted = true;
            }
            Step::PostFix(_) => return Err("v1 fix results cannot carry thread replies".into()),
            Step::PostProof => {
                return Err("proof output awaits independently attested GitHub evidence".into());
            }
            Step::StopJob(id) => {
                let work = record
                    .jobs
                    .iter()
                    .find(|w| &w.job.id == id)
                    .ok_or("job missing")?;
                jobs::cancel(work)?;
            }
            Step::StartDeferred(names) => {
                super::deferred::begin(pass, github, f, names, record, data)?
            }
            Step::CancelDeferred(nonce) => {
                super::deferred::cancel(github, f, nonce, pass.now, record, data)?
            }
            Step::UpdateBranch => github.update(&repo, f.number, &f.revision.head)?,
            Step::Merge => {
                // Read back the permission; a refused or lost status write must never merge.
                let statuses = github.statuses(&repo, &f.revision.head)?;
                let status = statuses.iter().find(|s| s.context == SETTLED);
                if !status.is_some_and(|s| {
                    s.state == "success"
                        && s.creator
                            .as_ref()
                            .is_some_and(|u| f.settings.review_authors.contains(&u.login))
                }) {
                    return Err("review-settled permission was not observed".into());
                }
                permission(pass, github, f, record)?;
                super::ledger::begin(f, record)?;
                let status = status.expect("permission checked above");
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
                match github.land(&repo, f.number, &f.revision.head)? {
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
                permission(pass, github, f, record)?;
                let store = store
                    .as_deref_mut()
                    .ok_or("train needs an anchored ledger")?;
                let config = crate::Config {
                    reviewers: f.settings.review_authors.iter().cloned().collect(),
                    own_contexts: vec![SETTLED.into(), crate::train::CONTEXT.into()],
                };
                let joined = crate::train::join(
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
                )?;
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
            Step::ListOwner => github.owner(&repo, f.number, &f.settings.owner_label)?,
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
        Step::UpdateBranch => Failure::Update,
        Step::Merge | Step::JoinTrain | Step::LeaveTrain => Failure::Merge,
        Step::CloseAlreadyLanded => Failure::Close,
        Step::PostReview(_) => Failure::Post,
        Step::PostFix(_) => Failure::Answer,
        Step::Prove | Step::PostProof => Failure::Build,
        Step::QueueReview(_) | Step::HandFix(_) | Step::Judge => Failure::Job,
        Step::StopJob(_) => Failure::Stop,
        Step::StartDeferred(_) | Step::CancelDeferred(_) => Failure::Deferred,
        _ => Failure::Status,
    }
}
fn author<G: Read>(github: &G, f: &Facts, record: &Record) -> Result<plugin::Identity> {
    for work in record.jobs.iter().rev() {
        if let Some(Ok(response)) = jobs::completion(work, jobs::now())?.result
            && let plugin::Answer::Fix {
                result: plugin::FixResult::Pushed { head },
            } = &response.answer
            && head == &f.revision.head
        {
            return Ok(response.by);
        }
    }
    let history = github.compare_commits(
        &f.repo.to_string(),
        f.base_tip.as_deref().ok_or("author base unknown")?,
        &f.revision.head,
    )?;
    if !history.complete {
        return Err("head author history is incomplete".into());
    }
    let mut engines = std::collections::BTreeSet::new();
    for commit in history.commits {
        let message = commit
            .commit
            .and_then(|c| c.message)
            .ok_or("head authorship unknown")?;
        let trailers = super::history::trailers(&message).ok_or("head trailers unreadable")?;
        engines.extend(
            trailers
                .into_iter()
                .filter(|(k, _)| k.eq_ignore_ascii_case("Engine"))
                .map(|(_, v)| v),
        );
    }
    let engine = match engines.len() {
        0 => {
            let pull = github.pull(&f.repo.to_string(), f.number)?;
            if pull.user.kind != "User" {
                return Err("automated head has no author engine".into());
            }
            pull.user.login
        }
        1 => engines.into_iter().next().ok_or("author engine missing")?,
        _ => return Err("v1 cannot establish independence from multiple author engines".into()),
    };
    // Session trailers are author-controlled text. Only a locally validated
    // producer response above can establish a known author session.
    Ok(plugin::Identity {
        engine,
        session: None,
    })
}

fn permission<G: Write>(pass: &Pass<'_>, github: &G, f: &Facts, record: &Record) -> Result<()> {
    let result = super::scope::current(pass, github, f, record)
        .and_then(|()| super::evidence::authorize(pass, github, f))
        .and_then(|()| super::scope::current(pass, github, f, record))
        .and_then(|()| {
            if super::approval::required(f) && !super::approval::approved(pass, github, f)? {
                return Err("An authorized person's scoped approval was withdrawn".into());
            }
            Ok(())
        });
    if result.is_err() {
        github.status(
            &f.repo.to_string(),
            &f.revision.head,
            false,
            "Permission inputs changed; read the PR again",
        )?;
    }
    result
}
