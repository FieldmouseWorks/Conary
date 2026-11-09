//! Re-read all permission inputs, including producer generations and worker
//! answers. Cosmetic controller labels are the only excluded PR metadata.
use super::persist::Result;
use super::*;

pub(crate) fn current<G: Write>(
    pass: &Pass<'_>,
    github: &G,
    f: &Facts,
    record: &Record,
) -> Result<()> {
    let mut fresh = read(
        github,
        &f.repo,
        f.number,
        pass.policy,
        pass.config.settings.clone(),
        &pass.config.trusted_authors,
    )?;
    super::observe::hydrate(&mut fresh, record, pass.now, true)?;
    super::observe::issuances(&mut fresh, &record.memory);
    let mut memory = record.memory.clone();
    super::ledger::raised(&mut fresh, &mut memory, pass.ledger);
    if fresh.changes_requested && !f.changes_requested {
        return Err("Review changes were requested before permission was used".into());
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
        || fresh.open_threads != f.open_threads
        || fresh.reviewers != f.reviewers
        || fresh.changes_requested != f.changes_requested
        || fresh.owner_approved != f.owner_approved
        || fresh.judgment != f.judgment
        || fresh.judgment_generation != f.judgment_generation
        || fresh.major != f.major
        || fresh.major_generation != f.major_generation
        || fresh.proof_data.records != f.proof_data.records
        || fresh.proof != f.proof
        || fresh.jobs != f.jobs
        || fresh.threads != f.threads
        || fresh.checks != f.checks
        || fresh.classified != f.classified
        || fresh.trusted != f.trusted
        || fresh.authorship_known != f.authorship_known
        || fresh.needs_judge != f.needs_judge
        || fresh.bot_edited != f.bot_edited
        || fresh.gate_edited != f.gate_edited
        || fresh.release_at != f.release_at
        || fresh.dependency != f.dependency
        || fresh.bot_merges != f.bot_merges
        || fresh.merging != f.merging
    {
        return Err("PR decision scope changed before permission or execution".into());
    }
    Ok(())
}
