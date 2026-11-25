//! GitHub effects not supplied by the existing train Forge.
use super::*;
use crate::api::ApiError;
use serde::{Deserialize, Serialize};
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
    fn stage(
        &self,
        repo: &str,
        pr: u64,
        stage: Stage,
        labels: &StageLabels,
    ) -> Result<(), ApiError>;
    /// Post a comment in the PR's conversation.
    fn comment(&self, repo: &str, pr: u64, body: &str) -> Result<(), ApiError>;
    /// Reply on review thread `thread`, by GitHub's id.
    fn reply(&self, repo: &str, pr: u64, thread: &str, body: &str) -> Result<(), ApiError>;
    /// Resolve review thread `thread`.
    fn resolve(&self, repo: &str, pr: u64, thread: &str) -> Result<(), ApiError>;
    fn update(&self, repo: &str, pr: u64, head: &str) -> Result<(), ApiError>;
    /// Run only the failed jobs of workflow run `run` again. Permitted only
    /// where single-attempt attestation cohesion is not required.
    fn rerun_failed_jobs(&self, repo: &str, run: u64) -> Result<(), ApiError>;
    /// Ask the App whose check suite `suite` is to run it again.
    fn rerequest(&self, repo: &str, suite: u64) -> Result<(), ApiError>;
    fn close(&self, repo: &str, pr: u64) -> Result<(), ApiError>;
    fn owner(&self, repo: &str, pr: u64, label: &str) -> Result<(), ApiError>;
    fn disable_auto_merge(&self, repo: &str, pr: u64) -> Result<(), ApiError>;
}

/// The label names the flow puts on a PR for each waiting stage, and takes
/// off when the stage changes. The defaults name no person or machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct StageLabels {
    /// The label on a PR at the `review` stage.
    #[schemars(extend("x-spender" = false))]
    pub review: String,
    /// The label on a PR at the `fix` stage.
    #[schemars(extend("x-spender" = false))]
    pub fix: String,
    /// The label on a PR at the `ci` stage.
    #[schemars(extend("x-spender" = false))]
    pub ci: String,
    /// The label on a PR at the `proofSlot` stage.
    #[schemars(extend("x-spender" = false))]
    pub proof_slot: String,
    /// The label on a PR at the `proof` stage.
    #[schemars(extend("x-spender" = false))]
    pub proof: String,
    /// The label on a PR at the `releaseAge` stage.
    #[schemars(extend("x-spender" = false))]
    pub release_age: String,
    /// The label on a PR at the `train` stage.
    #[schemars(extend("x-spender" = false))]
    pub train: String,
    /// The label on a PR at the `merge` stage.
    #[schemars(extend("x-spender" = false))]
    pub merge: String,
    /// The label on a PR at the `owner` stage.
    #[schemars(extend("x-spender" = false))]
    pub owner: String,
    /// The label on a PR at the `base` stage.
    #[schemars(extend("x-spender" = false))]
    pub base: String,
}

impl Default for StageLabels {
    fn default() -> Self {
        Self {
            review: "waiting: review".into(),
            fix: "waiting: fix".into(),
            ci: "waiting: CI".into(),
            proof_slot: "waiting: proof slot".into(),
            proof: "waiting: machine checks".into(),
            release_age: "waiting: release age".into(),
            train: "waiting: train".into(),
            merge: "waiting: merge".into(),
            owner: "waiting: owner".into(),
            base: "waiting: base branch".into(),
        }
    }
}

impl StageLabels {
    /// The configured label for `stage`, or `None` for the null stage.
    pub fn label(&self, stage: Stage) -> Option<&str> {
        Some(match stage {
            Stage::Review => &self.review,
            Stage::Fix => &self.fix,
            Stage::Ci => &self.ci,
            Stage::ProofSlot => &self.proof_slot,
            Stage::Proof => &self.proof,
            Stage::ReleaseAge => &self.release_age,
            Stage::Train => &self.train,
            Stage::Merge => &self.merge,
            Stage::Owner => &self.owner,
            Stage::Base => &self.base,
            Stage::None => return None,
        })
    }

    /// Whether `label` is one of the configured stage labels.
    pub fn owns(&self, label: &str) -> bool {
        self.stage(label).is_some()
    }

    /// The stage `label` names under this configuration.
    pub fn stage(&self, label: &str) -> Option<Stage> {
        Stage::LABELLED
            .into_iter()
            .find(|s| self.label(*s) == Some(label))
    }

    pub(crate) fn entries(&self) -> [(&'static str, &str); 10] {
        [
            ("review", &self.review),
            ("fix", &self.fix),
            ("ci", &self.ci),
            ("proof_slot", &self.proof_slot),
            ("proof", &self.proof),
            ("release_age", &self.release_age),
            ("train", &self.train),
            ("merge", &self.merge),
            ("owner", &self.owner),
            ("base", &self.base),
        ]
    }
}

impl Settings {
    /// Check that every stage label is non-empty, distinct from every other
    /// stage label (case-insensitively, as GitHub treats label names), and
    /// distinct from the other labels the flow reads or puts on PRs.
    /// Error messages name only options, never values (`schema::Consistent`).
    pub(crate) fn validate_stage_labels(&self) -> super::persist::Result<()> {
        let mut seen: Vec<(&str, &str)> = Vec::with_capacity(Stage::LABELLED.len());
        for (field, value) in self.stage_labels.entries() {
            if value.trim().is_empty() {
                return Err(format!("settings.stage_labels.{field} must not be empty").into());
            }
            if let Some((earlier, _)) = seen
                .iter()
                .find(|(_, existing)| existing.eq_ignore_ascii_case(value))
            {
                return Err(format!(
                    "settings.stage_labels.{field} collides with settings.stage_labels.{earlier}"
                )
                .into());
            }
            if value.eq_ignore_ascii_case(&self.hold_label) {
                return Err(format!(
                    "settings.stage_labels.{field} collides with settings.hold_label"
                )
                .into());
            }
            if self
                .owner_label
                .as_deref()
                .is_some_and(|owner| value.eq_ignore_ascii_case(owner))
            {
                return Err(format!(
                    "settings.stage_labels.{field} collides with settings.owner_label"
                )
                .into());
            }
            if self
                .priority_labels
                .keys()
                .any(|priority| value.eq_ignore_ascii_case(priority))
            {
                return Err(format!(
                    "settings.stage_labels.{field} collides with settings.priority_labels"
                )
                .into());
            }
            seen.push((field, value));
        }
        Ok(())
    }
}

impl Stage {
    /// Every stage that has a label, in stages.py's order.
    pub const LABELLED: [Stage; 10] = [
        Self::Review,
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
