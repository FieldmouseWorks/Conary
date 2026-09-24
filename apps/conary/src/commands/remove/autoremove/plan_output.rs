// apps/conary/src/commands/remove/autoremove/plan_output.rs
//! Typed JSON projection of an autoremove dry-run plan.

use anyhow::Result;
use conary_agent_contract::{OperationEnvelope, OperationStatus, PlanResult, RiskLevel};
use conary_core::db::models::Trove;

use super::AutoremovePlan;

pub(super) const AUTOREMOVE_PLAN_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
pub(super) struct AutoremovePackage {
    pub name: String,
    pub version: String,
    pub architecture: Option<String>,
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
    pub removable: Vec<AutoremovePackage>,
    pub skipped: Vec<AutoremoveSkippedPackage>,
}

impl AutoremovePackage {
    fn from_trove(trove: &Trove) -> Self {
        Self {
            name: trove.name.clone(),
            version: trove.version.clone(),
            architecture: trove.architecture.clone(),
        }
    }
}

impl AutoremovePlanData {
    pub(super) fn from_plan(plan: &AutoremovePlan) -> Self {
        Self {
            schema_version: AUTOREMOVE_PLAN_SCHEMA_VERSION,
            removable: plan
                .removable
                .iter()
                .map(AutoremovePackage::from_trove)
                .collect(),
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
}

pub(super) fn plan_result(data: &AutoremovePlanData) -> Result<PlanResult> {
    let risk = if data.removable.is_empty() {
        RiskLevel::ReadOnly
    } else {
        RiskLevel::Destructive
    };
    let summary = format!(
        "{} orphaned package(s) would be removed; {} skipped",
        data.removable.len(),
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
