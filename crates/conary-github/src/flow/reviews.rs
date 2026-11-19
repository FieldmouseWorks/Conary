//! What the reviewer said of a PR's head, read from what Codex's GitHub
//! connector (the ChatGPT Codex Connector App) posts on the PR: the summary
//! comment it keeps up to date, whose `codex-review-state:v1` mark names the
//! commit it last reviewed, whether that review is done and whether it found
//! anything; and a review with inline findings for each look that found
//! some, its receipt naming the commit it read. Only posts by the review
//! authors count, since anyone can write the marks. burrow's steward reads
//! them the same way (burrow-upkeep's `verdict`).
//!
//! The connector reviews each push itself, so nothing carries from an
//! earlier head: a head is clean only once a review of that exact commit
//! finished with nothing found. Its word binds only the commit, though: a
//! retarget onto another base is another diff, and an edited description is
//! text a review may not have read, and nothing the connector posts says
//! which base or description it read, or when a review began. So a PR that
//! changed either way at or after its head's push is `changed`, and a judge,
//! whose verdict binds all three, decides it instead.
use super::{Reviewer, Settings, Verdict};
use crate::api::{ApiError, Comment, Review as GithubReview};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::LazyLock;

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
    let t = text
        .parse::<jiff::Timestamp>()
        .map_err(|_| ApiError::new("GitHub returned an invalid timestamp"))?
        .as_second();
    u64::try_from(t).map_err(|_| ApiError::new("GitHub returned a pre-epoch timestamp"))
}

/// When the PR last changed without a push, and when its head was pushed,
/// in Unix seconds, as GitHub dates them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Since {
    /// Its title's or body's last edit.
    pub edited: Option<u64>,
    /// Its last retarget.
    pub retargeted: Option<u64>,
    /// Its head's push, as near as GitHub says: the earliest check suite on
    /// it. None when nothing checks it.
    pub pushed: Option<u64>,
}

impl Since {
    /// Whether it was retargeted or edited at or after its push, or at all
    /// when the push time isn't known.
    pub fn changed(&self) -> bool {
        let since = self.edited.max(self.retargeted).unwrap_or(0);
        since > 0 && self.pushed.is_none_or(|p| since >= p)
    }
}

/// The comment that asks the connector for a review of the head, as its
/// docs give it, and why.
pub(super) fn ask(why: &str) -> String {
    format!("@codex review\n\n{why}")
}

/// The summary comment's state for one kind of review.
#[derive(serde::Deserialize)]
struct State {
    commit_sha: Option<String>,
    status: Option<String>,
    has_any_findings: Option<bool>,
}

static STATE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?s)<!-- codex-review-state:v1 (\{.*?\}) -->").expect("pattern")
});
static RECEIPT: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"Reviewed commit:\*\*\s*`([0-9a-f]{7,40})`").expect("pattern")
});

/// What the reviewer last said of `head`, for each of the settings'
/// reviewers. Fails on a time GitHub gave that isn't one.
pub(super) fn read(
    head: &str,
    records: &[GithubReview],
    comments: &[Comment],
    since: Since,
    settings: &Settings,
) -> Result<BTreeMap<String, Reviewer>, ApiError> {
    let theirs = |login: Option<&str>| login.is_some_and(|l| settings.review_authors.contains(l));
    let when = |t: Option<&str>| match t {
        None | Some("") => Ok(0),
        Some(t) => timestamp(t),
    };
    let mut said = Reviewer {
        changed: since.changed(),
        ..Reviewer::default()
    };
    let mut saw = |verdict, at: u64, body: &str| {
        if at >= said.at {
            said.verdict = Some(verdict);
            said.at = at;
            said.source = Some(body.to_owned());
        }
    };
    for c in comments {
        if !theirs(c.user.as_ref().map(|u| u.login.as_str())) {
            continue;
        }
        let Some(body) = c.body.as_deref() else {
            continue;
        };
        let Some(m) = STATE.captures(body) else {
            continue;
        };
        let Ok(mut all) = serde_json::from_str::<serde_json::Value>(&m[1]) else {
            continue;
        };
        let Ok(code) = serde_json::from_value::<State>(all["code_review"].take()) else {
            continue;
        };
        if code.commit_sha.as_deref() != Some(head) {
            continue;
        }
        let verdict = match (code.status.as_deref(), code.has_any_findings) {
            (Some("completed"), Some(false)) => Verdict::Clean,
            (Some("completed"), Some(true)) => Verdict::Findings,
            // Begun, or a state this doesn't know: not a verdict yet.
            _ => Verdict::Running,
        };
        saw(verdict, when(c.updated_at.as_deref())?, body);
    }
    let mut rounds = 0u32;
    for r in records {
        if !theirs(r.user.as_ref().map(|u| u.login.as_str())) {
            continue;
        }
        let Some(body) = r.body.as_deref() else {
            continue;
        };
        let Some(read) = RECEIPT.captures(body) else {
            continue;
        };
        rounds = rounds.saturating_add(1);
        // A receipt names only a prefix of the commit, so it counts only on
        // the commit the review is on: findings there, never clean (the
        // summary says that).
        if r.commit_id.as_deref() == Some(head) && head.starts_with(&read[1]) {
            saw(Verdict::Findings, when(r.submitted_at.as_deref())?, body);
        }
    }
    said.rounds = rounds;
    Ok(settings
        .reviewers
        .iter()
        .map(|name| (name.clone(), said.clone()))
        .collect())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::User;

    const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const OLD: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const CONNECTOR: &str = "chatgpt-codex-connector[bot]";

    fn settings() -> Settings {
        Settings {
            review_authors: [CONNECTOR.into()].into(),
            ..Settings::default()
        }
    }
    fn by(login: &str) -> Option<User> {
        Some(User {
            login: login.into(),
            kind: "Bot".into(),
        })
    }
    /// The connector's summary comment, as it keeps it, for a review of `of`.
    fn summary(of: &str, status: &str, findings: Option<bool>, at: &str) -> Comment {
        let state = serde_json::json!({
            "code_review": {"commit_sha": of, "has_any_findings": findings, "has_unresolved_findings": findings, "status": status},
            "security_review": {"commit_sha": null, "has_any_findings": null, "has_unresolved_findings": null, "status": null},
        });
        Comment {
            user: by(CONNECTOR),
            body: Some(format!(
                "<!-- codex-pull-request-review-summary -->\n<!-- codex-review-state:v1 {state} -->\n\n## Codex Review Summary"
            )),
            created_at: Some(at.into()),
            updated_at: Some(at.into()),
        }
    }
    /// The connector's review with findings on `on`, its receipt naming `named`.
    fn receipt(on: &str, named: &str, at: &str) -> GithubReview {
        GithubReview {
            id: Some(1),
            state: Some("COMMENTED".into()),
            user: by(CONNECTOR),
            body: Some(format!(
                "<!-- codex-review-receipt:v1 -->\n\n### Codex Review\n\n**Reviewed commit:** `{named}`\n"
            )),
            commit_id: Some(on.into()),
            html_url: None,
            submitted_at: Some(at.into()),
        }
    }
    fn said(reviews: &[GithubReview], comments: &[Comment]) -> (Option<Verdict>, u32) {
        let r = &read(HEAD, reviews, comments, Since::default(), &settings()).unwrap()["reviewer"];
        (r.verdict, r.rounds)
    }

    #[test]
    fn a_summary_says_running_clean_or_findings_of_its_commit_only() {
        const T: &str = "2026-11-18T00:00:00Z";
        assert_eq!(said(&[], &[]), (None, 0));
        for (status, findings, verdict) in [
            ("completed", Some(false), Verdict::Clean),
            ("completed", Some(true), Verdict::Findings),
            ("running", None, Verdict::Running),
            // A state this doesn't know is no verdict yet.
            ("completed", None, Verdict::Running),
        ] {
            assert_eq!(
                said(&[], &[summary(HEAD, status, findings, T)]),
                (Some(verdict), 0)
            );
        }
        assert_eq!(
            said(&[], &[summary(OLD, "completed", Some(false), T)]),
            (None, 0)
        );
    }

    #[test]
    fn a_receipt_counts_a_round_and_is_findings_only_on_its_own_commit() {
        const T: &str = "2026-11-18T00:00:00Z";
        assert_eq!(
            said(&[receipt(HEAD, &HEAD[..10], T)], &[]),
            (Some(Verdict::Findings), 1)
        );
        // Named another commit than the one it's on, or on an older one.
        assert_eq!(said(&[receipt(HEAD, &OLD[..10], T)], &[]), (None, 1));
        assert_eq!(said(&[receipt(OLD, &OLD[..10], T)], &[]), (None, 1));
    }

    #[test]
    fn the_newest_word_on_the_head_wins() {
        let findings = receipt(HEAD, &HEAD[..10], "2026-11-18T00:00:00Z");
        let clean = summary(HEAD, "completed", Some(false), "2026-11-18T01:00:00Z");
        assert_eq!(said(&[findings], &[clean]), (Some(Verdict::Clean), 1));
        let findings = receipt(HEAD, &HEAD[..10], "2026-11-18T02:00:00Z");
        let clean = summary(HEAD, "completed", Some(false), "2026-11-18T01:00:00Z");
        assert_eq!(said(&[findings], &[clean]), (Some(Verdict::Findings), 1));
    }

    /// What the connector posted on two PRs, as GitHub's REST API gave it
    /// (its own posts only): remi#1409's summary and clean receipt, and a
    /// findings review on burrow#1217.
    #[test]
    fn reads_the_connectors_posts_as_github_gives_them() {
        let comments: Vec<Comment> =
            serde_json::from_str(include_str!("reviews/remi-1409-comments.json")).unwrap();
        let clean = &read(
            "34131866953a65cf56c0c2d01f5329959b4e338d",
            &[],
            &comments,
            Since::default(),
            &settings(),
        )
        .unwrap()["reviewer"];
        // Its clean receipt is a PR comment, not a review: no round.
        assert_eq!((clean.verdict, clean.rounds), (Some(Verdict::Clean), 0));
        assert_eq!(clean.at, timestamp("2026-11-19T03:52:54Z").unwrap());
        let reviews: Vec<GithubReview> =
            serde_json::from_str(include_str!("reviews/burrow-1217-reviews.json")).unwrap();
        let findings = &read(
            "3fcbe7a9347244680d1271ed8f7d5ea797247bcc",
            &reviews,
            &[],
            Since::default(),
            &settings(),
        )
        .unwrap()["reviewer"];
        assert_eq!(
            (findings.verdict, findings.rounds),
            (Some(Verdict::Findings), 1)
        );
        // On the head a later push left, it's a round and no verdict.
        let later = &read(
            "ed80c8841e88fb8848f117a3036f0fd4fc4b38ea",
            &reviews,
            &[],
            Since::default(),
            &settings(),
        )
        .unwrap()["reviewer"];
        assert_eq!((later.verdict, later.rounds), (None, 1));
    }

    #[test]
    fn a_pr_changed_at_or_after_its_push_is_changed() {
        let since = |edited, retargeted, pushed| Since {
            edited,
            retargeted,
            pushed,
        };
        for changed in [
            since(Some(2), None, Some(1)),
            since(None, Some(2), Some(1)),
            since(None, Some(1), Some(1)),
            since(Some(1), None, None),
        ] {
            assert!(changed.changed(), "{changed:?}");
        }
        for unchanged in [
            since(Some(1), Some(1), Some(2)),
            since(None, None, None),
            since(None, None, Some(2)),
        ] {
            assert!(!unchanged.changed(), "{unchanged:?}");
        }
    }
}
