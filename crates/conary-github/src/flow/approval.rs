//! A label is a queue signal, never a person's authorization.
use super::persist::Result;
use super::*;
use std::collections::BTreeMap;

/// The authorized person posts this exact scope in an APPROVED GitHub review.
/// GitHub authenticates its account and commit; the body also binds intent
/// and target, which a head-only review cannot establish after a retarget.
pub fn owner_approval(repo: &conary_core::op::Repo, pr: u64, revision: &Revision) -> String {
    format!(
        "<!-- conary-owner:v1 {} -->",
        serde_json::json!({
            "repo":repo.to_string().to_lowercase(),"pr":pr,"revision":revision
        })
    )
}
pub(crate) fn required(f: &Facts) -> bool {
    f.classified.class == conary_core::Class::R2
        || f.judgment
            .as_ref()
            .is_some_and(|j| j.revision == f.revision && j.verdict == Judgment::Owner)
}
pub(crate) fn approved<G: Read>(pass: &Pass<'_>, github: &G, f: &Facts) -> Result<bool> {
    if super::read::excluded(f) || !required(f) {
        return Ok(false);
    }
    let records = github.reviews(&f.repo.to_string(), f.number)?;
    Ok(from_reviews(
        &f.repo,
        f.number,
        &f.revision,
        &records,
        &pass.config.settings.people,
    ))
}
pub(crate) fn from_reviews(
    repo: &conary_core::op::Repo,
    pr: u64,
    revision: &Revision,
    records: &[crate::api::Review],
    people: &std::collections::BTreeSet<String>,
) -> bool {
    let mut latest = BTreeMap::new();
    for review in records {
        let Some(user) = &review.user else {
            continue;
        };
        if user.kind != "User" || !people.contains(&user.login) {
            continue;
        }
        let Some(id) = review.id else {
            continue;
        };
        if latest.get(&user.login).is_none_or(|(old, _)| *old < id) {
            latest.insert(user.login.clone(), (id, review));
        }
    }
    let marker = owner_approval(repo, pr, revision);
    latest.values().any(|(_, r)| {
        r.state.as_deref() == Some("APPROVED")
            && r.commit_id.as_deref() == Some(&revision.head)
            && r.body
                .as_deref()
                .is_some_and(|b| b.lines().any(|line| line == marker))
    })
}
