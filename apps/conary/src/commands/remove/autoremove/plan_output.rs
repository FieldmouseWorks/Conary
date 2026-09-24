// apps/conary/src/commands/remove/autoremove/plan_output.rs
//! Typed JSON projection of an autoremove dry-run plan.

use anyhow::Result;
use conary_agent_contract::{OperationEnvelope, OperationStatus, PlanResult, RiskLevel};
use conary_core::db::models::Trove;

use super::AutoremoveFixedPointPlan;

pub(super) const AUTOREMOVE_PLAN_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
pub(super) struct AutoremovePackage {
    pub name: String,
    pub version: String,
    pub package_release: Option<String>,
    pub architecture: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
pub(super) struct AutoremoveRemovablePackage {
    #[serde(flatten)]
    pub package: AutoremovePackage,
    pub round: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
pub(super) struct AutoremoveSkippedPackage {
    #[serde(flatten)]
    pub package: AutoremovePackage,
    pub reason: AutoremoveSkipReason,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum AutoremoveSkipReason {
    AdoptedNativeAuthority,
    Pinned,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
pub(super) struct AutoremovePlanData {
    pub schema_version: u32,
    pub removable: Vec<AutoremoveRemovablePackage>,
    pub skipped: Vec<AutoremoveSkippedPackage>,
}

impl AutoremovePackage {
    fn from_trove(trove: &Trove) -> Self {
        Self {
            name: trove.name.clone(),
            version: trove.version.clone(),
            package_release: trove.package_release.clone(),
            architecture: trove.architecture.clone(),
        }
    }
}

impl AutoremovePlanData {
    pub(super) fn from_fixed_point(plan: &AutoremoveFixedPointPlan) -> Self {
        let mut removable = Vec::new();
        for round in &plan.rounds {
            for trove in &round.removable {
                removable.push(AutoremoveRemovablePackage {
                    package: AutoremovePackage::from_trove(trove),
                    round: round.round,
                });
            }
        }

        Self {
            schema_version: AUTOREMOVE_PLAN_SCHEMA_VERSION,
            removable,
            skipped: plan
                .skipped
                .iter()
                .map(|(trove, reason)| AutoremoveSkippedPackage {
                    package: AutoremovePackage::from_trove(trove),
                    reason: reason.clone(),
                })
                .collect(),
        }
    }

    /// Number of removal rounds in this plan.
    ///
    /// The planner records a round only when it holds removable troves and
    /// numbers them consecutively from 1, so the highest round is the count.
    fn round_count(&self) -> u32 {
        self.removable
            .iter()
            .map(|package| package.round)
            .max()
            .unwrap_or(0)
    }
}

pub(super) fn plan_result(data: &AutoremovePlanData) -> Result<PlanResult> {
    let risk = if data.removable.is_empty() {
        RiskLevel::ReadOnly
    } else {
        RiskLevel::Destructive
    };
    let summary = format!(
        "{} orphaned package(s) would be removed in {} round(s); {} skipped",
        data.removable.len(),
        data.round_count(),
        data.skipped.len()
    );
    let envelope = OperationEnvelope::new(
        "package.autoremove.plan",
        OperationStatus::Planned,
        risk,
        summary,
    );
    Ok(PlanResult::new(envelope).with_data(serde_json::to_value(data)?))
}
