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
    let mut prepare = || -> Result<()> {
        let repo = f.repo.to_string();
        let settled = matches!(
            next.step,
            Step::Merge | Step::Settle | Step::JoinTrain | Step::CloseAlreadyLanded
        ) || next.stage == Stage::Train
            && f.train.seat.is_some()
            && !matches!(next.step, Step::LeaveTrain | Step::Retry { .. });
        // Revoke before any fallible cosmetic work, including Retry passes.
        github.status(&repo, &f.revision.head, false, &next.description)?;
        scope(pass, github, f)?;
        if matches!(next.step, Step::Merge | Step::Settle)
            && f.classified
                .clone()
                .raise(record.memory.raised.iter().cloned().collect())
                .class
                == conary_core::Class::R2
        {
            return Err("R2 landing requires the candidate train; configure train mode".into());
        }
        if settled {
            let negative = pass
                .config
                .negative
                .as_ref()
                .ok_or("negative evidence verifier is not configured")?
                .verifier()?;
            let config = crate::Config {
                reviewers: f.settings.review_authors.iter().cloned().collect(),
                own_contexts: vec![SETTLED.into(), crate::train::CONTEXT.into()],
            };
            crate::train::verify_negative(
                store
                    .as_deref_mut()
                    .ok_or("negative evidence needs an anchored ledger")?,
                github,
                github,
                &config,
                &f.repo,
                f.number,
                &f.revision.head,
                &negative,
            )?
            .map_err(|why| format!("negative evidence: {why}"))?;
        }
        github.stage(&repo, f.number, next.stage)?;
        if settled {
            scope(pass, github, f)?;
            github.status(&repo, &f.revision.head, true, &next.description)?;
            guarded_scope(pass, github, f)?;
        }
        Ok(())
    };
    prepare().map_err(|error| (Failure::Status, error))?;
    let run = || -> Result<Vec<Failure>> {
        let repo = f.repo.to_string();
        match &next.step {
            Step::Wait { .. } | Step::Retry { .. } | Step::Settle => {}
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
                let mut problems = Vec::new();
                for work in &record.jobs {
                    if !matches!(work.purpose, Purpose::Review(_))
                        || work.job.revision != f.revision
                    {
                        continue;
                    }
                    if let Some(Ok(plugin::Response {
                        answer: plugin::Answer::Review { findings },
                        ..
                    })) = jobs::completion(work, pass.now)?.result
                    {
                        problems.extend(
                            findings
                                .into_iter()
                                .map(|finding| plugin::Problem::Finding { finding }),
                        );
                    }
                }
                problems.extend(f.checks.iter().filter(|c| c.state == Ci::Red).map(|c| {
                    plugin::Problem::RedCheck {
                        check: c.name.clone(),
                        text: "GitHub check failed".into(),
                    }
                }));
                if *kind == Kind::Conflict {
                    problems.push(plugin::Problem::Conflict {
                        base: f.base_tip.clone().ok_or("conflict base tip unknown")?,
                        text: "GitHub reports a merge conflict".into(),
                    });
                }
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
                guarded_scope(pass, github, f)?;
                match github.land(&repo, f.number, &f.revision.head)? {
                    crate::train::PrMerge::Merged => {}
                    crate::train::PrMerge::Refused(why) => return Err(why.into()),
                }
            }
            Step::JoinTrain => {
                guarded_scope(pass, github, f)?;
                let store = store.ok_or("train needs an anchored ledger")?;
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
                            .negative
                            .as_ref()
                            .ok_or("negative evidence verifier is not configured")?
                            .verifier()?,
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
                    store.ok_or("train needs an anchored ledger")?,
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
    run().map_err(|error| (kind, error))
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

/// Revalidate policy inputs that can change without a push. Stage labels are
/// controller output; all other labels remain decision inputs.
fn scope<G: Read>(pass: &Pass<'_>, github: &G, f: &Facts) -> Result<()> {
    let current = read(
        github,
        &f.repo,
        f.number,
        pass.policy,
        f.settings.clone(),
        &pass.config.trusted_authors,
    )?;
    let labels = |facts: &Facts| {
        facts
            .labels
            .iter()
            .filter(|label| {
                ![
                    Stage::Review,
                    Stage::Reviewer,
                    Stage::Fix,
                    Stage::Ci,
                    Stage::ProofSlot,
                    Stage::Proof,
                    Stage::ReleaseAge,
                    Stage::Train,
                    Stage::Merge,
                    Stage::Owner,
                    Stage::Base,
                ]
                .iter()
                .any(|stage| stage.label() == Some(label.as_str()))
            })
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
    };
    if current.revision != f.revision
        || current.open != f.open
        || current.draft != f.draft
        || current.trusted != f.trusted
        || current.complete != f.complete
        || labels(&current) != labels(f)
        || current.base_tip != f.base_tip
        || current.classified != f.classified
        || current.checks != f.checks
        || current.has_ci != f.has_ci
        || current.open_threads != f.open_threads
        || current.changes_requested != f.changes_requested
    {
        return Err("PR scope moved before executing flow decision".into());
    }
    Ok(())
}

fn guarded_scope<G: Write>(pass: &Pass<'_>, github: &G, f: &Facts) -> Result<()> {
    if let Err(error) = scope(pass, github, f) {
        github.status(
            &f.repo.to_string(),
            &f.revision.head,
            false,
            "Decision scope changed; settlement revoked",
        )?;
        return Err(error);
    }
    Ok(())
}
