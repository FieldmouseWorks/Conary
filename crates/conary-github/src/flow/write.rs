//! GitHub effects not supplied by the existing train Forge.
use super::*;
use crate::api::ApiError;
use serde_json::Value;
pub const SETTLED: &str = "review-settled";
pub trait Write: Read + crate::train::Forge {
    /// Start a workflow on `git_ref` (a branch) with string inputs. GitHub
    /// answers 204 with no run: the run is found afterwards by its inputs.
    fn dispatch(
        &self,
        repo: &str,
        workflow: &str,
        git_ref: &str,
        inputs: &std::collections::BTreeMap<String, String>,
    ) -> Result<(), ApiError>;
    fn cancel_run(&self, repo: &str, run: u64) -> Result<(), ApiError>;
    fn status(&self, repo: &str, head: &str, settled: bool, text: &str) -> Result<(), ApiError>;
    fn stage(&self, repo: &str, pr: u64, stage: Stage) -> Result<(), ApiError>;
    fn review(&self, repo: &str, pr: u64, head: &str, body: &str) -> Result<(), ApiError>;
    fn update(&self, repo: &str, pr: u64, head: &str) -> Result<(), ApiError>;
    fn close(&self, repo: &str, pr: u64) -> Result<(), ApiError>;
    fn owner(&self, repo: &str, pr: u64, label: &str) -> Result<(), ApiError>;
    fn disable_auto_merge(&self, repo: &str, pr: u64) -> Result<(), ApiError>;
}
impl Stage {
    /// Every stage that has a label, in stages.py's order.
    pub const LABELLED: [Stage; 11] = [
        Self::Review,
        Self::Reviewer,
        Self::Fix,
        Self::Ci,
        Self::ProofSlot,
        Self::Proof,
        Self::ReleaseAge,
        Self::Train,
        Self::Merge,
        Self::Owner,
        Self::Base,
    ];

    pub fn owns_label(label: &str) -> bool {
        Self::from_label(label).is_some()
    }

    /// The stage a `waiting: …` label names.
    pub fn from_label(label: &str) -> Option<Stage> {
        Self::LABELLED
            .into_iter()
            .find(|s| s.label() == Some(label))
    }

    pub fn label(self) -> Option<&'static str> {
        Some(match self {
            Self::Review => "waiting: review",
            Self::Reviewer => "waiting: reviewer",
            Self::Fix => "waiting: fix",
            Self::Ci => "waiting: CI",
            Self::ProofSlot => "waiting: proof slot",
            Self::Proof => "waiting: machine checks",
            Self::ReleaseAge => "waiting: release age",
            Self::Train => "waiting: train",
            Self::Merge => "waiting: merge",
            Self::Owner => "waiting: Peter",
            Self::Base => "waiting: base branch",
            Self::None => return None,
        })
    }
}
pub fn review_body(
    role: &str,
    revision: &Revision,
    at: u64,
    findings: &[conary_core::plugin::Finding],
) -> String {
    let mut body = findings
        .iter()
        .map(|f| format!("{}:{}: {:?}: {}", f.path, f.line, f.severity, f.text))
        .collect::<Vec<_>>()
        .join("\n\n");
    if body.is_empty() {
        body = "Review found no findings.".into();
    }
    let marker: Value = serde_json::json!({
        "commit_sha":revision.head, "base":revision.base, "scope":revision.description,
        "reviewed_at":at, "findings":findings.len(), "problems":findings
    });
    format!("{body}\n\n<!-- {role}-review:v2 {marker} -->")
}
