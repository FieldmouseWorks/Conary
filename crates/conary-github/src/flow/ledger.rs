//! Append-only classification and landing reconciliation use the existing
//! ingestion path. Local pending intents survive a lost merge response.
use super::persist::{Landing, Result};
use super::*;
use conary_core::{Ledger, Store};

pub(crate) fn config(settings: &Settings) -> crate::Config {
    crate::Config {
        reviewers: settings.review_authors.iter().cloned().collect(),
        own_contexts: vec![SETTLED.into(), crate::train::CONTEXT.into()],
    }
}
pub(crate) fn raised(f: &mut Facts, memory: &mut Memory, ledger: Option<&Ledger>) {
    if let Some(change) = ledger.and_then(|l| l.by_pr(&f.repo, f.number)) {
        memory.raised.extend(change.raised().iter().cloned());
    }
    memory.raised.extend(f.classified.raised.iter().cloned());
    f.classified = f
        .classified
        .clone()
        .raise(memory.raised.iter().cloned().collect());
}
pub(crate) fn classify<G: Write>(
    pass: &Pass<'_>,
    github: &G,
    f: &mut Facts,
    record: &mut Record,
    store: &mut Store,
) -> Result<()> {
    raised(f, &mut record.memory, Some(store.ledger()));
    let pull = github.pull(&f.repo.to_string(), f.number)?;
    let receipts = submissions(record);
    let got = crate::ingest::submitted(
        store,
        github,
        &config(&f.settings),
        &f.repo,
        &pull,
        &receipts,
    )?;
    if Revision::from(&pull) != f.revision {
        return Err("PR moved while recording its classification".into());
    }
    let classes = crate::class::of(
        store,
        github,
        pass.policy,
        &f.repo,
        got.change,
        got.revision,
        record.memory.raised.iter().cloned().collect(),
    )?;
    if !super::read::excluded(f) {
        f.classified = classes.said;
    }
    raised(f, &mut record.memory, Some(store.ledger()));
    Ok(())
}
/// Revisit pending records even after their PR has disappeared from Open.
/// Ingestion verifies the actual GitHub merge and historical permission;
/// a nondefault target closes without inventing a trunk landing.
pub(crate) fn reconcile<G: Write>(
    pass: &Pass<'_>,
    github: &G,
    repo: &conary_core::op::Repo,
    data: &Data,
    store: &mut Store,
    pr: u64,
    record: &mut Record,
) -> Result<()> {
    if record.landing.is_some() || !record.pending.is_empty() {
        let pull = github.pull(&repo.to_string(), pr)?;
        let receipts = submissions(record);
        crate::ingest::submitted(
            store,
            github,
            &config(&pass.config.settings),
            repo,
            &pull,
            &receipts,
        )?;
        let finished = store.ledger().by_pr(repo, pr).is_some_and(|c| {
            matches!(
                c.status(),
                conary_core::ledger::Status::Landed
                    | conary_core::ledger::Status::Reverted
                    | conary_core::ledger::Status::Closed
            )
        });
        for intent in record.landing.iter_mut().chain(&mut record.pending) {
            if !intent.delegated && !intent.accepted && pull.state == "open" && !pull.is_merged() {
                intent.retry = retryable(github, repo, pr, &pull, intent)?;
            }
        }
        data.save(pr, record)?;
        if finished || pull.state != "open" && !pull.is_merged() {
            record.landing = None;
            record.pending.clear();
            data.save(pr, record)?;
        }
    }
    Ok(())
}
fn submissions(record: &Record) -> Vec<crate::ingest::Submission> {
    record
        .landing
        .iter()
        .chain(&record.pending)
        .filter_map(|l| {
            Some(crate::ingest::Submission {
                head: l.head.clone(),
                target: l.target.clone(),
                before: l.base.clone(),
                description: l.revision.as_ref()?.description.clone(),
                permission: l.permission.clone()?,
            })
        })
        .collect()
}
pub(crate) fn intent(f: &Facts, delegated: bool) -> Result<Landing> {
    Ok(Landing {
        permission: None,
        head: f.revision.head.clone(),
        target: f.revision.base.clone(),
        revision: Some(f.revision.clone()),
        base: f.base_tip.clone().ok_or("landing base unknown")?,
        delegated,
        accepted: false,
        retry: false,
    })
}
pub(crate) fn cleanup<G: Write>(
    github: &G,
    repo: &conary_core::op::Repo,
    pr: u64,
    data: &Data,
    record: &mut Record,
) -> Result<()> {
    for head in record.cleanup.clone() {
        github.status(&repo.to_string(), &head, false, crate::ingest::CLEANUP)?;
        record.cleanup.remove(&head);
        data.save(pr, record)?;
    }
    Ok(())
}

/// Reconcile the original target using the complete comparison, including
/// merges through non-first parents. A changed head fences an old request;
/// retain its observation even if history is incomplete while permitting
/// the superseding revision to make progress.
fn retryable<G: Write>(
    github: &G,
    repo: &conary_core::op::Repo,
    pr: u64,
    pull: &crate::api::PullRequest,
    intent: &Landing,
) -> Result<bool> {
    let full = repo.to_string();
    let target = if intent.target.is_empty() {
        &pull.base.name
    } else {
        &intent.target
    };
    let superseded = pull.head.sha != intent.head;
    let Some(tip) = github.branch(&full, target)? else {
        return Ok(superseded);
    };
    let history = github.compare_commits(&full, &intent.base, &tip)?;
    for commit in &history.commits {
        if github.commit_pulls(&full, &commit.sha)?.contains(&pr) {
            return Ok(false);
        }
    }
    Ok(superseded || history.complete)
}
pub(crate) fn begin(f: &Facts, record: &mut Record) -> Result<()> {
    let new = intent(f, false)?;
    if let Some(old) = record.landing.take()
        && (old.head != new.head
            || old.target != new.target
            || old.revision != new.revision
            || old.base != new.base)
        && !record.pending.iter().any(|p| {
            p.head == old.head
                && p.target == old.target
                && p.revision == old.revision
                && p.base == old.base
        })
    {
        record.pending.push(old);
    }
    record.landing = Some(new);
    Ok(())
}

pub(crate) fn withdraw_live(pr: u64, data: &Data, record: &mut Record) -> Result<()> {
    if !record.live.is_empty() {
        record.cleanup.append(&mut record.live);
        data.save(pr, record)?;
    }
    Ok(())
}
