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
    let (got, pull) =
        crate::ingest::pulled(store, github, &config(&f.settings), &f.repo, f.number)?;
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
    if record.landing.is_some() {
        let pull = github.pull(&repo.to_string(), pr)?;
        crate::ingest::pull(store, github, &config(&pass.config.settings), repo, pr)?;
        let finished = store.ledger().by_pr(repo, pr).is_some_and(|c| {
            matches!(
                c.status(),
                conary_core::ledger::Status::Landed
                    | conary_core::ledger::Status::Reverted
                    | conary_core::ledger::Status::Closed
            )
        });
        if let Some(intent) = record.landing.as_mut()
            && !intent.delegated
            && !intent.accepted
            && pull.state == "open"
            && !pull.is_merged()
        {
            intent.retry = retryable(github, repo, pr, &pull.base.name, intent)?;
            data.save(pr, record)?;
        }
        if finished || pull.state != "open" && !pull.is_merged() {
            record.landing = None;
            data.save(pr, record)?;
        }
    }
    Ok(())
}
pub(crate) fn intent(f: &Facts, delegated: bool) -> Result<Landing> {
    Ok(Landing {
        head: f.revision.head.clone(),
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
        github.status(
            &repo.to_string(),
            &head,
            false,
            "Revoke unconfirmed landing permission",
        )?;
        record.cleanup.remove(&head);
        data.save(pr, record)?;
    }
    Ok(())
}

/// A known acceptance is never retried. For an uncertain submission, an
/// expected-head retry is safe only while no merge of this PR is visible on
/// the target's first-parent history. The pending observation is retained
/// even while another fully revalidated attempt is allowed.
fn retryable<G: Write>(
    github: &G,
    repo: &conary_core::op::Repo,
    pr: u64,
    target: &str,
    intent: &Landing,
) -> Result<bool> {
    let full = repo.to_string();
    let mut at = github
        .branch(&full, target)?
        .ok_or("landing target disappeared")?;
    for _ in 0..300 {
        if at == intent.base {
            return Ok(true);
        }
        if github.commit_pulls(&full, &at)?.contains(&pr) {
            return Ok(false);
        }
        let commit = github.commit(&full, &at)?;
        let Some(parent) = commit.parents.first() else {
            return Ok(false);
        };
        at = parent.sha.clone();
    }
    Ok(false)
}

pub(crate) fn withdraw_live(pr: u64, data: &Data, record: &mut Record) -> Result<()> {
    if !record.live.is_empty() {
        record.cleanup.append(&mut record.live);
        data.save(pr, record)?;
    }
    Ok(())
}
