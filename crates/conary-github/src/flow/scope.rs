//! Scope checks bracket permission publication and its use. Expected-head
//! merging remains the final forge-side protection against a racing push.
use super::persist::Result;
use super::*;

pub(crate) fn current<G: Write>(github: &G, f: &Facts) -> Result<()> {
    let repo = f.repo.to_string();
    let pull = github.pull(&repo, f.number)?;
    let extra = github.snapshot(&repo, f.number)?;
    let labels: std::collections::BTreeSet<_> = pull
        .labels
        .iter()
        .filter(|l| !Stage::owns_label(&l.name))
        .map(|l| l.name.clone())
        .collect();
    let expected: std::collections::BTreeSet<_> = f
        .labels
        .iter()
        .filter(|l| !Stage::owns_label(l))
        .cloned()
        .collect();
    if Revision::from(&pull) != f.revision
        || extra.revision != f.revision
        || (pull.state == "open") != f.open
        || extra.draft != f.draft
        || extra.complete != f.complete
        || labels != expected
        || extra.open_threads != f.open_threads
        || (f.base_tip.is_some() && github.branch(&repo, &pull.base.name)? != f.base_tip)
    {
        // If the change raced the success publication itself, withdraw that
        // permission before returning the stale decision as a failed effect.
        github.status(
            &repo,
            &f.revision.head,
            false,
            "Decision scope changed; read the PR again",
        )?;
        return Err("PR decision scope changed before permission or execution".into());
    }
    Ok(())
}
