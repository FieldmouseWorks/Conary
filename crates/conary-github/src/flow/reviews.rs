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
    let description = fingerprint(pull.body.as_deref().unwrap_or(""));
    let mut result = BTreeMap::new();
    for name in &settings.reviewers {
        let pattern = regex::Regex::new(&format!(
            r"<!-- {}-review:v1 (\{{.*?\}}) -->",
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
            let Some(m) = r
                .body
                .as_deref()
                .and_then(|b| pattern.captures(b))
                .and_then(|c| serde_json::from_str::<serde_json::Value>(&c[1]).ok())
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
            let Some(head) = r.commit_id.as_deref() else {
                continue;
            };
            if m["commit_sha"].as_str() != Some(head)
                || head != revision.head && !carried.contains(head)
                || m["base"].as_str() != Some(revision.base.as_str())
                || m["description"].as_str() != Some(description.as_str())
            {
                continue;
            }
            let at = match r.submitted_at.as_deref() {
                None | Some("") => 0,
                Some(t) => timestamp(t)?,
            };
            if candidates.get(head).is_none_or(|(v, _, _)| at >= v.at) {
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
        let chosen = candidates.remove(&revision.head).or_else(|| {
            candidates
                .into_values()
                .filter(|(v, _, _)| v.verdict == Verdict::Clean)
                .max_by_key(|(v, _, _)| v.at)
        });
        if let Some((review, id, answered)) = chosen {
            reviewer.review = Some(review);
            reviewer.record_id = id;
            reviewer.findings_answered = answered;
        }
        result.insert(name.clone(), reviewer);
    }
    Ok(result)
}
