//! Review marks are data only when the configured steward posted them on
//! the commit they name. Availability is independent local service state.
use super::{Review, Reviewer, Revision, Settings, Verdict};
use crate::api::{ApiError, PullRequest, Review as GithubReview};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub fn fingerprint(body: &str) -> String {
    let quoted = if body.trim().is_empty() {
        "(none)".into()
    } else {
        body.trim()
            .lines()
            .map(|l| format!("> {l}").trim_end().to_owned())
            .collect::<Vec<_>>()
            .join("\n")
    };
    Sha256::digest(quoted.as_bytes())[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub(crate) fn timestamp(text: &str) -> Result<u64, ApiError> {
    let t = chrono::DateTime::parse_from_rfc3339(text)
        .map_err(|_| ApiError::new("GitHub returned an invalid timestamp"))?
        .timestamp();
    u64::try_from(t).map_err(|_| ApiError::new("GitHub returned a pre-epoch timestamp"))
}

pub(super) fn read(
    pull: &PullRequest,
    records: &[GithubReview],
    resolved: &BTreeSet<u64>,
    settings: &Settings,
    carried: &BTreeSet<String>,
) -> Result<BTreeMap<String, Reviewer>, ApiError> {
    let revision = Revision::from(pull);
    let description = &revision.description;
    let mut result = BTreeMap::new();
    for name in &settings.reviewers {
        let pattern = regex::Regex::new(&format!(
            r"(?m)^<!-- {}-review:v([12]) (\{{[^\n]*\}}) -->\s*\z",
            regex::escape(name)
        ))
        .expect("escaped pattern");
        let mut reviewer = Reviewer::default();
        let mut candidates = BTreeMap::<String, (Review, Option<u64>, bool)>::new();
        for r in records.iter().filter(|r| {
            r.user
                .as_ref()
                .is_some_and(|u| settings.review_authors.contains(&u.login))
        }) {
            let Some((version, m)) = r
                .body
                .as_deref()
                .and_then(|b| pattern.captures(b))
                .and_then(|c| {
                    serde_json::from_str::<serde_json::Value>(&c[2])
                        .ok()
                        .map(|mark| (c[1].to_owned(), mark))
                })
            else {
                continue;
            };
            let count = match m.get("findings") {
                None => 0,
                Some(n) => match n.as_u64() {
                    Some(n) => n,
                    None => continue,
                },
            };
            if count > 0 {
                reviewer.rounds = reviewer.rounds.saturating_add(1);
            }
            // Legacy findings still count toward the historical cap. Their
            // body-only scope cannot establish coverage of today's claim.
            if version != "2" {
                continue;
            }
            let Some(head) = r.commit_id.as_deref() else {
                continue;
            };
            if m["commit_sha"].as_str() != Some(head)
                || head != revision.head && !carried.contains(head)
                || m["base"].as_str() != Some(revision.base.as_str())
                || m["scope"].as_str() != Some(description.as_str())
            {
                continue;
            }
            let posted_at = match r.submitted_at.as_deref() {
                None | Some("") => {
                    return Err(ApiError::new(
                        "GitHub returned a scoped review without a submission time",
                    ));
                }
                Some(t) => timestamp(t)?,
            };
            // Posting can be delayed or retried. Only the producer's completion
            // time orders verdicts; neither a new REST ID nor posting time does.
            let at = m["reviewed_at"]
                .as_u64()
                .ok_or_else(|| ApiError::new("Scoped review lacks its producer completion time"))?;
            if at > posted_at {
                return Err(ApiError::new(
                    "Review completion is later than its submission",
                ));
            }
            if candidates.get(head).is_none_or(|(v, id, _)| {
                (at, count > 0, r.id.unwrap_or(0))
                    >= (v.at, v.verdict == Verdict::Findings, id.unwrap_or(0))
            }) {
                candidates.insert(
                    head.into(),
                    (
                        Review {
                            revision: revision.clone(),
                            verdict: if count > 0 {
                                Verdict::Findings
                            } else {
                                Verdict::Clean
                            },
                            at,
                        },
                        r.id,
                        count > 0 && r.id.is_some_and(|id| resolved.contains(&id)),
                    ),
                );
            }
        }
        let chosen = candidates
            .remove(&revision.head)
            .map(|v| (revision.head.clone(), v))
            .or_else(|| {
                candidates.into_iter().max_by_key(|(_, (v, id, _))| {
                    (v.at, v.verdict == Verdict::Findings, id.unwrap_or(0))
                })
            });
        if let Some((head, (review, id, answered))) = chosen {
            reviewer.reviewed_head = Some(head);
            reviewer.review = Some(review);
            reviewer.record_id = id;
            reviewer.findings_answered = answered;
        }
        result.insert(name.clone(), reviewer);
    }
    Ok(result)
}

/// The latest native review state per account, shared by reading and the
/// final admission check so a same-head request for changes cannot race in.
pub(super) fn changes_requested(records: &[GithubReview]) -> bool {
    let mut verdicts = BTreeMap::new();
    for r in records {
        if let (Some(who), Some(state)) = (&r.user, &r.state)
            && matches!(
                state.as_str(),
                "APPROVED" | "CHANGES_REQUESTED" | "DISMISSED"
            )
        {
            verdicts.insert(&who.login, state);
        }
    }
    verdicts.values().any(|s| s.as_str() == "CHANGES_REQUESTED")
}
