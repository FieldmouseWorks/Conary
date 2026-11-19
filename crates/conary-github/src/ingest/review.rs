//! The reviewer's word on a PR's head, recorded as `review` results that
//! nothing attests (DESIGN.md 4.3): what Codex's GitHub connector posts, and
//! the markers the steward put on the reviews it posted before.
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

/// What the reviewer said of this head, as the flow reads it
/// ([`crate::flow::reviews::words`]): each finished verdict, a pass for
/// clean and a fail for findings, recorded unattested, since nothing
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
    let (words, _) = reviews::words(&pr.head.sha, reviews, comments, |login| {
        config.review_authors.iter().any(|a| a == login)
    })?;
    Ok(words
        .into_iter()
        .filter(|w| since.is_none_or(|s| w.at >= s))
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
                check: reviews::NAME.to_owned(),
                result,
                source: w.url.map_or_else(
                    || format!("{} on PR {}", reviews::NAME, pr.number),
                    str::to_owned,
                ),
                attest: None,
            })
        })
        .collect())
}
