//! Causal permission retained for a submitted, expected-head merge.
use super::*;
/// A controller-observed permission saved before its expected-head merge
/// request. It authorizes only that exact revision on that exact trunk tip.
#[derive(Debug, Clone)]
pub(crate) struct Submission {
    pub head: String,
    pub target: String,
    pub before: String,
    pub description: String,
    pub permission: Permission,
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct Permission {
    pub login: String,
    pub at: String,
    pub text: Option<String>,
}
pub(crate) const CLEANUP: &str = "Revoke unconfirmed landing permission";

pub(super) fn authority(
    source: &dyn Source,
    config: &Config,
    repo: &Repo,
    pr: &PullRequest,
    statuses: &[CommitStatus],
    since: Option<&str>,
    submissions: &[Submission],
) -> Result<Option<Authority>, IngestError> {
    let full = repo.to_string();
    let before_merge = |at: Option<&str>| {
        pr.merged_at
            .as_deref()
            .is_some_and(|m| at.is_some_and(|t| t <= m))
    };
    let current = |at: Option<&str>| since.is_none_or(|s| at.is_some_and(|t| t >= s));
    // A cleanup after a submitted request revokes future permission, not
    // the request already sent. GitHub timestamps cannot order these events
    // within a second. Use the saved causal observation, authenticated again
    // against GitHub history, and require the actual original trunk tip.
    if pr.is_merged() && pr.targets_trunk() {
        for receipt in submissions {
            if receipt.head != pr.head.sha
                || receipt.target != pr.base.name
                || receipt.description
                    != crate::flow::fingerprint(&format!(
                        "{}\n{}",
                        pr.title,
                        pr.body.as_deref().unwrap_or_default()
                    ))
                || !config.reviewers.contains(&receipt.permission.login)
            {
                continue;
            }
            let relevant: Vec<_> = statuses
                .iter()
                .filter(|s| {
                    config.own_contexts.contains(&s.context)
                        && s.creator
                            .as_ref()
                            .is_some_and(|u| config.reviewers.contains(&u.login))
                })
                .collect();
            let observed = relevant.iter().position(|s| {
                s.state == "success"
                    && s.created_at.as_deref() == Some(receipt.permission.at.as_str())
                    && s.description == receipt.permission.text
                    && s.creator
                        .as_ref()
                        .is_some_and(|u| u.login == receipt.permission.login)
            });
            let Some(index) = observed else { continue };
            if !before_merge(Some(&receipt.permission.at))
                || !current(Some(&receipt.permission.at))
                || relevant[..index]
                    .iter()
                    .any(|s| s.state != "pending" || s.description.as_deref() != Some(CLEANUP))
            {
                continue;
            }
            let Some(sha) = pr.merge_commit_sha.as_deref() else {
                continue;
            };
            let landed = source.commit(&full, sha)?;
            if trunk_before(source, &full, pr.number, &landed)?.as_str() == receipt.before {
                return Ok(Some(Authority::Status {
                    login: receipt.permission.login.clone(),
                }));
            }
        }
    }
    Ok(None)
}
