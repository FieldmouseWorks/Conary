//! What the reviewer said of a PR's head, read through the configured
//! [`ReviewAdapter`] from what `settings.review_authors` post on the PR.
//!
//! Only posts by the configured review authors count, since anyone can write
//! HTML comment marks. The reviewer's word binds only the commit: a retarget
//! onto another base is another diff, and an edited description is text a
//! review may not have read, so a PR that changed either way at or after its
//! head's push is `changed`, and a judge decides it instead.
mod codex;
mod marker;

use std::collections::BTreeMap;

use conary_core::plugin::Severity;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{Reviewer, Settings, Verdict};
use crate::api::{ApiError, Comment, Review as GithubReview};

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

/// Protocol contract for a GitHub-posting reviewer adapter: how to ask it for
/// a review, how to read its verdicts and rounds from PR reviews and comments,
/// how to parse a finding's severity from a review thread's opening comment,
/// and the check name its word is recorded under in the ledger.
pub(crate) trait ReviewerAdapter<'a> {
    fn check_name(&self) -> &'a str;
    fn ask_comment(&self) -> Option<&'a str>;
    fn words<'b>(
        &self,
        head: &str,
        records: &'b [GithubReview],
        comments: &'b [Comment],
        authors: &dyn Fn(&str) -> bool,
    ) -> Result<Said<'b>, ApiError>;
    fn severity(&self, body: &str) -> Severity;
}

/// Configuration for a custom marker-based GitHub reviewer adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CustomReviewAdapter {
    /// Check name recorded in the ledger for this reviewer's verdicts.
    #[schemars(extend("x-spender" = false))]
    pub check_name: String,
    /// PR comment body posted to request a review when no `review_ask` door
    /// covers the repository; `null` disables comment triggers.
    #[schemars(extend("x-spender" = false))]
    #[serde(default)]
    pub ask_comment: Option<String>,
    /// HTML comment marker name for summary state payloads (`<!-- <state_marker> {...} -->`).
    #[schemars(extend("x-spender" = false))]
    pub state_marker: String,
    /// HTML comment marker name for review receipt payloads (`<!-- <receipt_marker> {...} -->`).
    #[schemars(extend("x-spender" = false))]
    pub receipt_marker: String,
}

/// Which GitHub-posting reviewer protocol the flow and ledger ingest speak.
/// The default (`conary`) is vendor-neutral; `codex` selects the built-in
/// ChatGPT Codex Connector adapter; `custom` configures arbitrary HTML comment
/// markers and trigger text.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReviewAdapter {
    /// Conary's vendor-neutral marker protocol (`conary-review-state:v1`,
    /// `conary-review-receipt:v1`, `/conary review`, and `[P0]`..`[P3]`).
    #[default]
    Conary,
    /// Built-in adapter for the ChatGPT Codex Connector GitHub App.
    Codex,
    /// Custom marker-based GitHub reviewer protocol.
    Custom(CustomReviewAdapter),
}

impl ReviewAdapter {
    /// Parse a CLI/env specification: `"conary"`, `"codex"`, or a JSON value
    /// matching [`ReviewAdapter`] (e.g. `{"custom": {...}}`).
    pub fn parse(spec: &str) -> std::result::Result<Self, String> {
        let adapter = match spec.trim() {
            "conary" => Self::Conary,
            "codex" => Self::Codex,
            other if other.starts_with('{') || other.starts_with('"') => {
                serde_json::from_str::<Self>(other)
                    .map_err(|_| "invalid review adapter JSON".to_owned())?
            }
            _ => {
                return Err(
                    "unknown review adapter: expected `conary`, `codex`, or a JSON adapter object"
                        .into(),
                );
            }
        };
        adapter.validate().map_err(|e| e.to_string())?;
        Ok(adapter)
    }

    /// Check that custom adapter fields are well-formed. Error messages name
    /// only options, never configured values (`schema::Consistent`).
    pub fn validate(&self) -> super::persist::Result<()> {
        let Self::Custom(c) = self else {
            return Ok(());
        };
        let valid_token =
            |s: &str| !s.is_empty() && !s.chars().any(|ch| ch.is_whitespace() || ch.is_control());
        if !valid_token(&c.check_name) {
            return Err(
                "settings.review_adapter.custom.check_name must be a non-empty token".into(),
            );
        }
        if c.ask_comment
            .as_deref()
            .is_some_and(|s| s.trim().is_empty())
        {
            return Err("settings.review_adapter.custom.ask_comment must not be blank".into());
        }
        if !valid_token(&c.state_marker) || c.state_marker.contains("-->") {
            return Err(
                "settings.review_adapter.custom.state_marker must be a non-empty marker name"
                    .into(),
            );
        }
        if !valid_token(&c.receipt_marker) || c.receipt_marker.contains("-->") {
            return Err(
                "settings.review_adapter.custom.receipt_marker must be a non-empty marker name"
                    .into(),
            );
        }
        if c.state_marker == c.receipt_marker {
            return Err(
                "settings.review_adapter.custom.receipt_marker must differ from settings.review_adapter.custom.state_marker"
                    .into(),
            );
        }
        Ok(())
    }

    fn with_adapter<'a, R>(&'a self, f: impl FnOnce(&dyn ReviewerAdapter<'a>) -> R) -> R {
        match self {
            Self::Conary => {
                let adapter = marker::MarkerAdapter::conary();
                f(&adapter)
            }
            Self::Codex => f(&codex::CodexAdapter),
            Self::Custom(c) => {
                let adapter = marker::MarkerAdapter {
                    check_name: &c.check_name,
                    ask_comment: c.ask_comment.as_deref(),
                    state_marker: &c.state_marker,
                    receipt_marker: &c.receipt_marker,
                };
                f(&adapter)
            }
        }
    }

    /// The check name recorded in the ledger for this adapter's verdicts.
    pub fn check_name(&self) -> &str {
        self.with_adapter(|a| a.check_name())
    }

    /// The PR comment body posted to request a review when no `review_ask`
    /// door covers the repository.
    pub fn ask_comment(&self) -> Option<&str> {
        self.with_adapter(|a| a.ask_comment())
    }

    /// Parse the severity of a review thread's opening comment under this
    /// adapter's convention. Threads without a recognized mark default to `P1`.
    pub fn severity(&self, body: &str) -> Severity {
        self.with_adapter(|a| a.severity(body))
    }

    /// Everything the accounts `authors` admits said of `head` under this
    /// adapter's protocol.
    pub(crate) fn words<'a>(
        &self,
        head: &str,
        records: &'a [GithubReview],
        comments: &'a [Comment],
        authors: impl Fn(&str) -> bool,
    ) -> Result<Said<'a>, ApiError> {
        self.with_adapter(|a| a.words(head, records, comments, &authors))
    }
}

/// One thing the reviewer said of a head: its verdict, when GitHub dates
/// the post that says it (Unix seconds, 0 undated), the post's body, and
/// its address when GitHub gave one.
pub(crate) struct Word<'a> {
    pub verdict: Verdict,
    pub at: u64,
    pub body: &'a str,
    pub url: Option<&'a str>,
}

/// Everything the reviewer said, as [`ReviewAdapter::words`] reads it.
pub(crate) struct Said<'a> {
    /// What it said of the head, its summary comments first and then its
    /// reviews, each as GitHub lists them.
    pub words: Vec<Word<'a>>,
    /// The rounds it took: its reviews with findings, at any head.
    pub rounds: u32,
    /// Why one of its marks didn't parse, if one didn't: it says nothing, and
    /// the flow's decision waits on it ([`super::degraded`]).
    pub unparsed: Option<&'static str>,
}

/// What the reviewer last said of `head`, for each of the settings'
/// reviewers: the newest of its [`ReviewAdapter::words`]. Fails on a time
/// GitHub gave that isn't one.
pub(super) fn read(
    head: &str,
    records: &[GithubReview],
    comments: &[Comment],
    since: Since,
    settings: &Settings,
    reads: &mut super::Reads,
) -> Result<BTreeMap<String, Reviewer>, ApiError> {
    let Said {
        words,
        rounds,
        unparsed,
    } = settings
        .review_adapter
        .words(head, records, comments, |l| {
            settings.review_authors.contains(l)
        })?;
    reads.mark(super::Mark::Review, unparsed);
    let mut said = Reviewer {
        changed: since.changed(),
        rounds,
        ..Reviewer::default()
    };
    for w in words {
        if w.at >= said.at {
            said.verdict = Some(w.verdict);
            said.at = w.at;
            said.source = Some(w.body.to_owned());
        }
    }
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
