//! Built-in reviewer adapter for the ChatGPT Codex Connector GitHub App:
//! its summary comment (`codex-review-state:v1`), its findings review receipt
//! (`**Reviewed commit:**`), its bare trigger comment (`@codex review`), and
//! its severity badges (`![P0 Badge]`..`![P3 Badge]`).
use std::sync::LazyLock;

use conary_core::plugin::Severity;

use super::{ReviewerAdapter, Said, Word, timestamp};
use crate::api::{ApiError, Comment, Review as GithubReview};
use crate::flow::Verdict;

/// The check name recorded in the ledger for this adapter's verdicts.
pub(crate) const NAME: &str = "codex";

/// The comment that asks the connector for a review of the head: the bare
/// trigger. The connector reads any text after it as a task and posts no
/// review, so the why stays in the flow's own record, never in the comment.
pub(crate) const ASK: &str = "@codex review";

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
static BADGE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"!\[P([0-3]) Badge\]").expect("pattern"));

/// Built-in adapter for the ChatGPT Codex Connector GitHub App.
pub(crate) struct CodexAdapter;

impl<'a> ReviewerAdapter<'a> for CodexAdapter {
    fn check_name(&self) -> &'a str {
        NAME
    }

    fn ask_comment(&self) -> Option<&'a str> {
        Some(ASK)
    }

    fn words<'b>(
        &self,
        head: &str,
        records: &'b [GithubReview],
        comments: &'b [Comment],
        authors: &dyn Fn(&str) -> bool,
    ) -> Result<Said<'b>, ApiError> {
        let theirs = |login: Option<&str>| login.is_some_and(authors);
        let when = |t: Option<&str>| match t {
            None | Some("") => Ok(0),
            Some(t) => timestamp(t),
        };
        let mut said = Vec::new();
        let mut unparsed = None;
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
                unparsed = Some("its summary's mark isn't JSON");
                continue;
            };
            // A summary with no code review in it says nothing of one.
            let code = all["code_review"].take();
            if code.is_null() {
                continue;
            }
            let Ok(code) = serde_json::from_value::<State>(code) else {
                unparsed = Some("its summary's code review isn't a review's state");
                continue;
            };
            if code.commit_sha.as_deref() != Some(head) {
                continue;
            }
            let verdict = match (code.status.as_deref(), code.has_any_findings) {
                (Some("completed"), Some(false)) => Verdict::Clean,
                (Some("completed"), Some(true)) => Verdict::Findings,
                (Some("running"), _) => Verdict::Running,
                // Ended with no word on findings (completed with none, failed,
                // skipped, cancelled), or a state this doesn't know: nothing is
                // under way, so it says nothing and the head is asked for again.
                // Waiting on it would wait forever.
                _ => continue,
            };
            said.push(Word {
                verdict,
                at: when(c.updated_at.as_deref())?,
                body,
                url: c.html_url.as_deref(),
            });
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
                said.push(Word {
                    verdict: Verdict::Findings,
                    at: when(r.submitted_at.as_deref())?,
                    body,
                    url: r.html_url.as_deref(),
                });
            }
        }
        Ok(Said {
            words: said,
            rounds,
            unparsed,
        })
    }

    fn severity(&self, body: &str) -> Severity {
        match BADGE
            .captures(body)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str())
        {
            Some("0") => Severity::P0,
            Some("2") => Severity::P2,
            Some("3") => Severity::P3,
            _ => Severity::P1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::User;
    use crate::flow::Settings;
    use crate::flow::reviews::{ReviewAdapter, Since, read};

    const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const OLD: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const CONNECTOR: &str = "chatgpt-codex-connector[bot]";

    fn settings() -> Settings {
        Settings {
            review_adapter: ReviewAdapter::Codex,
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
            html_url: None,
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
        let r = &read(
            HEAD,
            reviews,
            comments,
            Since::default(),
            &settings(),
            &mut Default::default(),
        )
        .unwrap()["reviewer"];
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
        ] {
            assert_eq!(
                said(&[], &[summary(HEAD, status, findings, T)]),
                (Some(verdict), 0)
            );
        }
        // Ended with no word on findings, or a state this doesn't know: no
        // review is under way, so it says nothing (#513).
        for (status, findings) in [
            ("completed", None),
            ("failed", None),
            ("skipped", None),
            ("cancelled", None),
            ("queued-somehow", Some(false)),
        ] {
            assert_eq!(
                said(&[], &[summary(HEAD, status, findings, T)]),
                (None, 0),
                "{status}"
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
        // A later summary with no word on findings doesn't hide them.
        let findings = receipt(HEAD, &HEAD[..10], "2026-11-18T00:00:00Z");
        let none = summary(HEAD, "completed", None, "2026-11-18T01:00:00Z");
        assert_eq!(said(&[findings], &[none]), (Some(Verdict::Findings), 1));
    }

    /// What the connector posted on two PRs, as GitHub's REST API gave it
    /// (its own posts only): remi#1409's summary and clean receipt, and a
    /// findings review on burrow#1217.
    #[test]
    fn reads_the_connectors_posts_as_github_gives_them() {
        let comments: Vec<Comment> =
            serde_json::from_str(include_str!("remi-1409-comments.json")).unwrap();
        let clean = &read(
            "34131866953a65cf56c0c2d01f5329959b4e338d",
            &[],
            &comments,
            Since::default(),
            &settings(),
            &mut Default::default(),
        )
        .unwrap()["reviewer"];
        // Its clean receipt is a PR comment, not a review: no round.
        assert_eq!((clean.verdict, clean.rounds), (Some(Verdict::Clean), 0));
        assert_eq!(clean.at, timestamp("2026-11-19T03:52:54Z").unwrap());
        let reviews: Vec<GithubReview> =
            serde_json::from_str(include_str!("burrow-1217-reviews.json")).unwrap();
        let findings = &read(
            "3fcbe7a9347244680d1271ed8f7d5ea797247bcc",
            &reviews,
            &[],
            Since::default(),
            &settings(),
            &mut Default::default(),
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
            &mut Default::default(),
        )
        .unwrap()["reviewer"];
        assert_eq!((later.verdict, later.rounds), (None, 1));
    }

    #[test]
    fn codex_badge_severity_parses_only_codex_badges() {
        assert_eq!(
            CodexAdapter.severity("![P0 Badge](https://example.invalid/p0)"),
            Severity::P0
        );
        assert_eq!(
            CodexAdapter.severity("![P2 Badge](https://example.invalid/p2)"),
            Severity::P2
        );
        assert_eq!(
            CodexAdapter.severity("![P3 Badge](https://example.invalid/p3)"),
            Severity::P3
        );
        // Neutral [P2] tags are not Codex badges: default to blocking P1.
        assert_eq!(CodexAdapter.severity("[P2] minor nit"), Severity::P1);
        assert_eq!(CodexAdapter.severity("plain person comment"), Severity::P1);
    }
}
