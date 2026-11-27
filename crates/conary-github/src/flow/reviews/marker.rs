//! Vendor-neutral marker-based GitHub reviewer adapter, used by both Conary's
//! default `"conary"` protocol and `"custom"` configured marker adapters:
//!
//! - Summary comment: `<!-- <state_marker> {"commit_sha":"...","status":"running"|"completed","has_any_findings":true|false|null} -->`
//! - Findings review receipt: `<!-- <receipt_marker> {"commit_sha":"..."} -->`
//! - Finding thread severity: `[P0]`..`[P3]` (defaulting to `P1`).
use std::sync::LazyLock;

use conary_core::plugin::Severity;

use super::{ReviewerAdapter, Said, Word, timestamp};
use crate::api::{ApiError, Comment, Review as GithubReview};
use crate::flow::Verdict;

/// Default vendor-neutral check name recorded in the ledger.
pub(crate) const CONARY_NAME: &str = "conary";
/// Default vendor-neutral trigger comment.
pub(crate) const CONARY_ASK: &str = "/conary review";
/// Default vendor-neutral summary comment marker.
pub(crate) const CONARY_STATE_MARKER: &str = "conary-review-state:v1";
/// Default vendor-neutral review receipt marker.
pub(crate) const CONARY_RECEIPT_MARKER: &str = "conary-review-receipt:v1";

static SEVERITY_TAG: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\[P([0-3])\]").expect("pattern"));

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct MarkerState {
    commit_sha: Option<String>,
    status: Option<String>,
    has_any_findings: Option<bool>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    commit_sha: String,
}

/// Marker-based reviewer protocol parameterized by check name, optional ask
/// comment, summary state marker name, and findings receipt marker name.
pub(crate) struct MarkerAdapter<'a> {
    pub check_name: &'a str,
    pub ask_comment: Option<&'a str>,
    pub state_marker: &'a str,
    pub receipt_marker: &'a str,
}

impl MarkerAdapter<'static> {
    pub(crate) const fn conary() -> Self {
        Self {
            check_name: CONARY_NAME,
            ask_comment: Some(CONARY_ASK),
            state_marker: CONARY_STATE_MARKER,
            receipt_marker: CONARY_RECEIPT_MARKER,
        }
    }
}

/// Extract the payload inside `<!-- <marker> <payload> -->`, without regex
/// compilation on configured marker names.
fn extract_marker<'a>(body: &'a str, marker: &str) -> Option<&'a str> {
    let prefix = format!("<!-- {marker} ");
    let start = body.find(&prefix)? + prefix.len();
    let rest = &body[start..];
    let end = rest.find(" -->")?;
    Some(rest[..end].trim())
}

impl<'a> ReviewerAdapter<'a> for MarkerAdapter<'a> {
    fn check_name(&self) -> &'a str {
        self.check_name
    }

    fn ask_comment(&self) -> Option<&'a str> {
        self.ask_comment
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
            let Some(payload) = extract_marker(body, self.state_marker) else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
                unparsed = Some("its summary's mark isn't JSON");
                continue;
            };
            let Ok(state) = serde_json::from_value::<MarkerState>(value) else {
                unparsed = Some("its summary's review mark isn't a review's state");
                continue;
            };
            if state.commit_sha.as_deref() != Some(head) {
                continue;
            }
            let verdict = match (state.status.as_deref(), state.has_any_findings) {
                (Some("completed"), Some(false)) => Verdict::Clean,
                (Some("completed"), Some(true)) => Verdict::Findings,
                (Some("running"), _) => Verdict::Running,
                // Ended with no word on findings, or unknown status: nothing
                // is under way, so the head can be asked for again.
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
            let Some(payload) = extract_marker(body, self.receipt_marker) else {
                continue;
            };
            let Ok(receipt) = serde_json::from_str::<Receipt>(payload) else {
                unparsed = Some("its review receipt mark isn't a review receipt");
                continue;
            };
            rounds = rounds.saturating_add(1);
            if r.commit_id.as_deref() == Some(head) && receipt.commit_sha == head {
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
        match SEVERITY_TAG
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
    use crate::flow::reviews::{CustomReviewAdapter, ReviewAdapter, Since, read};

    const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const OLD: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const BOT: &str = "acme-reviewer[bot]";

    fn by(login: &str) -> Option<User> {
        Some(User {
            login: login.into(),
            kind: "Bot".into(),
        })
    }
    fn summary(marker: &str, of: &str, status: &str, findings: Option<bool>, at: &str) -> Comment {
        let state = serde_json::json!({
            "commit_sha": of,
            "status": status,
            "has_any_findings": findings,
        });
        Comment {
            user: by(BOT),
            body: Some(format!("<!-- {marker} {state} -->\n\nReview Summary")),
            created_at: Some(at.into()),
            updated_at: Some(at.into()),
            html_url: None,
        }
    }
    fn receipt(marker: &str, on: &str, named: &str, at: &str) -> GithubReview {
        let payload = serde_json::json!({"commit_sha": named});
        GithubReview {
            id: Some(1),
            state: Some("COMMENTED".into()),
            user: by(BOT),
            body: Some(format!("<!-- {marker} {payload} -->\n\nFindings.")),
            commit_id: Some(on.into()),
            html_url: None,
            submitted_at: Some(at.into()),
        }
    }

    #[test]
    fn conary_adapter_reads_neutral_marks_and_ignores_codex_marks() {
        const T: &str = "2026-11-18T00:00:00Z";
        let settings = Settings {
            review_authors: [BOT.into()].into(),
            ..Settings::default()
        };
        assert_eq!(settings.review_adapter, ReviewAdapter::Conary);
        assert_eq!(settings.review_adapter.check_name(), "conary");
        assert_eq!(
            settings.review_adapter.ask_comment(),
            Some("/conary review")
        );

        // Codex summary comment and receipt are ignored under default Conary adapter.
        let codex_state = serde_json::json!({
            "code_review": {"commit_sha": HEAD, "status": "completed", "has_any_findings": false}
        });
        let codex_comment = Comment {
            user: by(BOT),
            body: Some(format!("<!-- codex-review-state:v1 {codex_state} -->")),
            created_at: Some(T.into()),
            updated_at: Some(T.into()),
            html_url: None,
        };
        let codex_review = GithubReview {
            id: Some(1),
            state: Some("COMMENTED".into()),
            user: by(BOT),
            body: Some(format!(
                "<!-- codex-review-receipt:v1 -->\n**Reviewed commit:** `{}`",
                &HEAD[..10]
            )),
            commit_id: Some(HEAD.into()),
            html_url: None,
            submitted_at: Some(T.into()),
        };
        let r = &read(
            HEAD,
            &[codex_review],
            &[codex_comment],
            Since::default(),
            &settings,
            &mut Default::default(),
        )
        .unwrap()["reviewer"];
        assert_eq!((r.verdict, r.rounds), (None, 0));

        // Conary neutral marks are read accurately.
        for (status, findings, expected) in [
            ("completed", Some(false), Some(Verdict::Clean)),
            ("completed", Some(true), Some(Verdict::Findings)),
            ("running", None, Some(Verdict::Running)),
            ("completed", None, None),
            ("failed", None, None),
        ] {
            let r = &read(
                HEAD,
                &[],
                &[summary(CONARY_STATE_MARKER, HEAD, status, findings, T)],
                Since::default(),
                &settings,
                &mut Default::default(),
            )
            .unwrap()["reviewer"];
            assert_eq!((r.verdict, r.rounds), (expected, 0), "{status}");
        }

        // Receipt counts a round on any head, and is Findings only when commit_id and commit_sha match head.
        let r = &read(
            HEAD,
            &[
                receipt(CONARY_RECEIPT_MARKER, OLD, OLD, T),
                receipt(CONARY_RECEIPT_MARKER, HEAD, HEAD, "2026-11-18T01:00:00Z"),
            ],
            &[],
            Since::default(),
            &settings,
            &mut Default::default(),
        )
        .unwrap()["reviewer"];
        assert_eq!((r.verdict, r.rounds), (Some(Verdict::Findings), 2));
    }

    #[test]
    fn custom_adapter_reads_only_its_configured_markers() {
        const T: &str = "2026-11-18T00:00:00Z";
        let settings = Settings {
            review_adapter: ReviewAdapter::Custom(CustomReviewAdapter {
                check_name: "acme".into(),
                ask_comment: Some("@acme review".into()),
                state_marker: "acme-review-state:v1".into(),
                receipt_marker: "acme-review-receipt:v1".into(),
            }),
            review_authors: [BOT.into()].into(),
            ..Settings::default()
        };
        // Conary's own default markers do not satisfy a custom adapter.
        let r = &read(
            HEAD,
            &[receipt(CONARY_RECEIPT_MARKER, HEAD, HEAD, T)],
            &[summary(
                CONARY_STATE_MARKER,
                HEAD,
                "completed",
                Some(false),
                T,
            )],
            Since::default(),
            &settings,
            &mut Default::default(),
        )
        .unwrap()["reviewer"];
        assert_eq!((r.verdict, r.rounds), (None, 0));

        // Configured custom markers do.
        let r = &read(
            HEAD,
            &[receipt("acme-review-receipt:v1", HEAD, HEAD, T)],
            &[summary(
                "acme-review-state:v1",
                HEAD,
                "completed",
                Some(false),
                "2026-11-18T01:00:00Z",
            )],
            Since::default(),
            &settings,
            &mut Default::default(),
        )
        .unwrap()["reviewer"];
        assert_eq!((r.verdict, r.rounds), (Some(Verdict::Clean), 1));
    }

    #[test]
    fn marker_severity_parses_neutral_tags_and_ignores_codex_badges() {
        let adapter = MarkerAdapter::conary();
        assert_eq!(adapter.severity("[P0] critical"), Severity::P0);
        assert_eq!(adapter.severity("[P1] bug"), Severity::P1);
        assert_eq!(adapter.severity("[P2] nit"), Severity::P2);
        assert_eq!(adapter.severity("[P3] style"), Severity::P3);
        // Codex shields.io badge is not recognized by the neutral adapter.
        assert_eq!(
            adapter.severity("![P2 Badge](https://img.shields.io/badge/P2-yellow)"),
            Severity::P1
        );
        assert_eq!(adapter.severity("plain person comment"), Severity::P1);
    }
}
