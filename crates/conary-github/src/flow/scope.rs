//! Re-read all permission inputs, including producer generations and worker
//! answers. Cosmetic controller labels are the only excluded PR metadata.
//! Non-granting cleanup also allows the expected exit of cancelled workers.
use super::persist::Result;
use super::*;
use conary_core::Ledger;

/// `ledger` is the ledger as it stands now, so a raise another writer
/// recorded since the decision read it withdraws the permission.
pub(crate) fn current<G: Write>(
    pass: &Pass<'_>,
    github: &G,
    f: &Facts,
    record: &Record,
    ledger: Option<&Ledger>,
) -> Result<()> {
    validate(pass, github, f, record, ledger, false)
}

/// The same PR scope check, allowing only the completion of work that was
/// pending at observation and is now marked cancelled. Remote inputs and
/// workers that were not cancelled are still compared freshly.
pub(crate) fn cleanup<G: Write>(
    pass: &Pass<'_>,
    github: &G,
    f: &Facts,
    record: &Record,
    ledger: Option<&Ledger>,
) -> Result<()> {
    validate(pass, github, f, record, ledger, true)
}
fn validate<G: Write>(
    pass: &Pass<'_>,
    github: &G,
    f: &Facts,
    record: &Record,
    ledger: Option<&Ledger>,
    cleanup: bool,
) -> Result<()> {
    // What degraded now that didn't when the decision read: what it
    // answered and the scope's read can't use withdraws the permission.
    let known = |d: &super::degraded::Degradation| {
        f.unreadable
            .iter()
            .chain(&f.reads.met)
            .any(|o| o.what == d.what)
    };
    // A read the decision can't go on without that fails now withdraws it
    // whatever it met before: the decision went on past it.
    let mut fresh = read(
        github,
        &f.repo,
        f.number,
        pass.policy,
        pass.config.settings.clone(),
        &pass.config.trusted_authors,
    )
    .map_err(|e| -> super::persist::Error {
        let mut new = vec![e.degradation.clone()];
        new.extend(e.met().into_iter().skip(1).filter(|d| !known(d)));
        Box::new(super::degraded::Withdrawn(new))
    })?;
    super::approval::labeller(github, &mut fresh, &record.memory);
    if cleanup {
        super::observe::hydrate_cleanup(&mut fresh, record, pass.now, &f.working)?;
    } else {
        super::observe::hydrate(&mut fresh, record, pass.now, true)?;
    }
    super::observe::issuances(&mut fresh, &record.memory);
    fresh.dispatches.clone_from(&record.dispatches);
    super::deferred::hydrate(github, &mut fresh, pass.config.evidence.as_ref(), pass.now)?;
    super::uncertain::hydrate(github, &mut fresh, pass.config.evidence.as_ref());
    let mut memory = record.memory.clone();
    super::ledger::raised(&mut fresh, &mut memory, ledger);
    if fresh.changes_requested && !f.changes_requested {
        return Err("Review changes were requested before permission was used".into());
    }
    // An answer the decision read that doesn't read now, or a read of
    // GitHub that answered it and fails or stops short now. Cleanup leaves
    // out stopped work it doesn't read, so only a new one counts.
    let new: Vec<_> = fresh
        .unreadable
        .iter()
        .chain(&fresh.reads.met)
        .filter(|d| !known(d))
        .cloned()
        .collect();
    if !new.is_empty() {
        return Err(Box::new(super::degraded::Withdrawn(new)));
    }
    let labels = |f: &Facts| {
        f.labels
            .iter()
            .filter(|l| !Stage::owns_label(l))
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
    };
    if fresh.revision != f.revision
        || fresh.base_tip != f.base_tip
        || fresh.open != f.open
        || fresh.draft != f.draft
        || fresh.complete != f.complete
        || labels(&fresh) != labels(f)
        || fresh.labelled_by != f.labelled_by
        || fresh.open_threads != f.open_threads
        || fresh.reviewers != f.reviewers
        || fresh.changes_requested != f.changes_requested
        || fresh.owner_approved != f.owner_approved
        || fresh.judgment != f.judgment
        || fresh.judgment_generation != f.judgment_generation
        || fresh.context() != f.context()
        || fresh.major != f.major
        || fresh.major_generation != f.major_generation
        || fresh.proof_data.records != f.proof_data.records
        || fresh.proof != f.proof
        || fresh.jobs != f.jobs
        || fresh.threads != f.threads
        || fresh.checks != f.checks
        || fresh.uncertain != f.uncertain
        || fresh.deferred != f.deferred
        || fresh.deferred_failures != f.deferred_failures
        || fresh.classified != f.classified
        || fresh.opened_by != f.opened_by
        || fresh.trusted != f.trusted
        || fresh.authorship_known != f.authorship_known
        || fresh.needs_judge != f.needs_judge
        || fresh.author != f.author
        || fresh.bot_edited != f.bot_edited
        || fresh.gate_edited != f.gate_edited
        || fresh.release_at != f.release_at
        || fresh.dependency != f.dependency
        || fresh.merging != f.merging
    {
        return Err("PR decision scope changed before permission or execution".into());
    }
    Ok(())
}
