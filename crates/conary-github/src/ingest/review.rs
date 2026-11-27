//! The reviewer's word on a PR's head, recorded as `review` results that
//! nothing attests (DESIGN.md 4.3): what the configured reviewer adapter reads
//! from `review_authors`'s posts, and the markers the steward put on the
//! reviews it posted before.
use std::sync::LazyLock;

use regex::Regex;

use conary_core::op::{Body, CheckKind, Outcome};
use conary_core::{ChangeId, RevisionId};

use super::{Config, IngestError};
use crate::api::{Comment, PullRequest, Review};

static MARKER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<!-- (\w+)-review:v1 (\{.*?\}) -->").unwrap());

#[derive(serde::Deserialize)]
struct Marker {
    commit_sha: String,
    base: String,
    findings: u64,
}

/// A reviewer's verdict, from the marker the steward put on each review it
/// posted before the reviewer posted its own:
/// `<!-- codex-review:v1 {"commit_sha":…,"base":…,"findings":N} -->`.
/// Only a marker on this head, against this base branch, counts as being
/// about this revision: a retargeted PR is another change on the same head.
pub(super) fn review_marker(
    config: &Config,
    pr: &PullRequest,
    change: ChangeId,
    revision: RevisionId,
    review: &Review,
) -> Option<Body> {
    let login = &review.user.as_ref()?.login;
    if !config.reviewers.contains(login) {
        return None;
    }
    let body = review
        .body
        .as_deref()?
        .replace("&#123;", "{")
        .replace("&#125;", "}");
    let m = MARKER.captures(&body)?;
    let said: Marker = serde_json::from_str(&m[2]).ok()?;
    if said.commit_sha != pr.head.sha
        || said.base != pr.base.name
        || review.commit_id.as_deref() != Some(pr.head.sha.as_str())
    {
        return None;
    }
    Some(Body::Evidence {
        change,
        revision,
        kind: CheckKind::Review,
        check: m[1].to_owned(),
        result: if said.findings == 0 {
            Outcome::Pass
        } else {
            Outcome::Fail
        },
        source: review
            .html_url
            .clone()
            .unwrap_or_else(|| format!("{} review by {login}", &m[1])),
        attest: None,
    })
}

/// What the reviewer said of this head, as the flow reads it through the
/// configured [`crate::flow::ReviewAdapter`]: each finished verdict, a pass
/// for clean and a fail for findings, recorded unattested, since nothing
/// attests a reviewer's word (DESIGN.md 4.3). Its posts name the commit and
/// not the base, so only those dated at or after the last retarget
/// (`since`) are about this revision.
pub(super) fn reviewer(
    config: &Config,
    pr: &PullRequest,
    change: ChangeId,
    revision: RevisionId,
    reviews: &[Review],
    comments: &[Comment],
    since: Option<&str>,
) -> Result<Vec<Body>, IngestError> {
    use crate::flow::{Verdict, reviews};
    let since = since.map(reviews::timestamp).transpose()?;
    // A summary whose mark didn't parse said nothing to record: the flow's
    // decision is what counts it failed.
    let said = config
        .review_adapter
        .words(&pr.head.sha, reviews, comments, |login| {
            config.review_authors.iter().any(|a| a == login)
        })?;
    let check_name = config.review_adapter.check_name();
    let mut words: Vec<_> = said
        .words
        .into_iter()
        .filter(|w| since.is_none_or(|s| w.at >= s))
        .collect();
    // Oldest first, so the ledger's last record of it is its last word.
    words.sort_by_key(|w| w.at);
    Ok(words
        .into_iter()
        .filter_map(|w| {
            let result = match w.verdict {
                Verdict::Clean => Outcome::Pass,
                Verdict::Findings => Outcome::Fail,
                Verdict::Running => return None,
            };
            Some(Body::Evidence {
                change,
                revision,
                kind: CheckKind::Review,
                check: check_name.to_owned(),
                result,
                source: publication(&w, check_name, pr.number),
                attest: None,
            })
        })
        .collect())
}

/// A word's publication, as the source its record names: where it is, and
/// when GitHub dated it. A reviewer may rewrite its summary comment in
/// place, so its address alone would make a later word the same record as
/// an earlier one with the same result, and the ledger would drop it.
fn publication(w: &crate::flow::reviews::Word<'_>, check_name: &str, pr: u64) -> String {
    let at = i64::try_from(w.at)
        .ok()
        .and_then(|s| jiff::Timestamp::from_second(s).ok())
        .map_or_else(|| w.at.to_string(), |t| t.to_string());
    match w.url {
        Some(url) => format!("{url} at {at}"),
        None => format!("{check_name} on PR {pr} at {at}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Ref, User};
    use crate::flow::{CustomReviewAdapter, ReviewAdapter};

    const HEAD: &str = "3333333333333333333333333333333333333333";
    const BASE: &str = "1111111111111111111111111111111111111111";
    const BOT: &str = "acme-reviewer[bot]";

    fn pr() -> PullRequest {
        PullRequest {
            number: 9,
            state: "open".into(),
            title: "change".into(),
            body: None,
            user: User {
                login: "owner".into(),
                kind: "User".into(),
            },
            created_at: None,
            updated_at: Some("2026-10-27T11:00:00Z".into()),
            merged: None,
            merged_at: None,
            merge_commit_sha: None,
            auto_merge: None,
            base: Ref {
                sha: BASE.into(),
                name: "main".into(),
                repo: None,
            },
            head: Ref {
                sha: HEAD.into(),
                name: "feature".into(),
                repo: None,
            },
            labels: vec![],
        }
    }

    #[test]
    fn non_codex_review_adapter_records_its_own_check_name_and_ignores_codex_marks() {
        let change = ChangeId::new().unwrap();
        let revision = RevisionId::new(&BASE.parse().unwrap(), &HEAD.parse().unwrap());
        let bot = || {
            Some(User {
                login: BOT.into(),
                kind: "Bot".into(),
            })
        };
        let reviews = vec![Review {
            id: Some(21),
            user: bot(),
            state: Some("COMMENTED".into()),
            body: Some(format!(
                r#"<!-- acme-review-receipt:v1 {{"commit_sha":"{HEAD}"}} -->"#
            )),
            commit_id: Some(HEAD.into()),
            html_url: Some("https://github.com/x/y/pull/9#pullrequestreview-21".into()),
            submitted_at: Some("2026-10-27T11:10:00Z".into()),
        }];
        let comments = vec![
            // Newer Codex-formatted comment must be ignored under the custom adapter.
            Comment {
                user: bot(),
                body: Some(format!(
                    r#"<!-- codex-review-state:v1 {{"code_review":{{"commit_sha":"{HEAD}","status":"completed","has_any_findings":true}}}} -->"#
                )),
                created_at: Some("2026-10-27T11:30:00Z".into()),
                updated_at: Some("2026-10-27T11:30:00Z".into()),
                html_url: Some("https://github.com/x/y/pull/9#issuecomment-1".into()),
            },
            Comment {
                user: bot(),
                body: Some(format!(
                    r#"<!-- acme-review-state:v1 {{"commit_sha":"{HEAD}","status":"completed","has_any_findings":false}} -->"#
                )),
                created_at: Some("2026-10-27T11:20:00Z".into()),
                updated_at: Some("2026-10-27T11:20:00Z".into()),
                html_url: Some("https://github.com/x/y/pull/9#issuecomment-22".into()),
            },
        ];
        let cfg = Config {
            reviewers: vec!["steward[bot]".into()],
            review_authors: vec![BOT.into()],
            review_adapter: ReviewAdapter::Custom(CustomReviewAdapter {
                check_name: "acme-review".into(),
                ask_comment: Some("@acme review".into()),
                state_marker: "acme-review-state:v1".into(),
                receipt_marker: "acme-review-receipt:v1".into(),
            }),
            own_contexts: vec!["review-settled".into()],
        };
        let bodies = reviewer(&cfg, &pr(), change, revision, &reviews, &comments, None).unwrap();
        let got: Vec<_> = bodies
            .iter()
            .map(|b| match b {
                Body::Evidence {
                    kind,
                    check,
                    result,
                    attest,
                    ..
                } => (*kind, check.as_str(), *result, attest.is_some()),
                other => panic!("unexpected body: {other:?}"),
            })
            .collect();
        assert_eq!(
            got,
            [
                (CheckKind::Review, "acme-review", Outcome::Fail, false),
                (CheckKind::Review, "acme-review", Outcome::Pass, false),
            ]
        );
    }
}
