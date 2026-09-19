// apps/conary-test/src/explorer/reducer.rs

use super::{
    contract::*,
    controller::{self, Campaign, Environment, Episode},
    evidence::Replay,
};
use anyhow::{Result, ensure};
use serde::Serialize;
use std::path::Path;
use std::sync::atomic::AtomicBool;

#[derive(Serialize)]
pub struct Reduction {
    pub version: u32,
    pub original: Replay,
    pub reduced: Replay,
    pub attempts: Vec<ReductionAttempt>,
    pub reason: String,
}
#[derive(Serialize)]
pub struct ReductionAttempt {
    pub removed_index: usize,
    pub evidence: String,
    pub reproduced: bool,
}

/// A bounded single-deletion reducer. No claim of global minimality.
pub async fn reduce(
    environment: &mut dyn Environment,
    campaign: &mut Campaign,
    original: &Replay,
    directory: &Path,
    cancel: &AtomicBool,
) -> Result<Reduction> {
    ensure!(
        !original.failure_signatures.is_empty(),
        "reduction requires a recorded failure predicate"
    );
    std::fs::create_dir(directory)?;
    let mut result = Reduction {
        version: VERSION,
        original: original.clone(),
        reduced: original.clone(),
        attempts: Vec::new(),
        reason: "bounded single-deletion search".into(),
    };
    let mut index = 0;
    while index < result.reduced.operations.len()
        && campaign.reductions < campaign.limits.reductions
    {
        if campaign.attempted >= campaign.limits.actions
            || cancel.load(std::sync::atomic::Ordering::SeqCst)
        {
            result.reason = "shared budget exhausted or cancelled".into();
            break;
        }
        campaign.reductions += 1;
        let mut candidate = result.reduced.clone();
        candidate.operations.remove(index);
        let name = format!("attempt-{}", campaign.reductions);
        let report = controller::run(
            environment,
            None,
            campaign,
            Episode {
                mode: Mode::Replay,
                identity: candidate.identity.clone(),
                replay: Some(&candidate),
                output: &directory.join(&name),
                cancel,
            },
        )
        .await?;
        let reproduced = report.reset_verified
            && report.cleanup == "removed"
            && matches!(
                report.stop_reason.as_str(),
                "recorded_trace_complete" | "selector_stop"
            )
            && !report.evaluations.iter().any(|e| e.passed.is_none())
            && report.signatures() == original.failure_signatures;
        result.attempts.push(ReductionAttempt {
            removed_index: index,
            evidence: name,
            reproduced,
        });
        if reproduced {
            result.reduced = report.replay();
        } else {
            index += 1;
        }
    }
    let summary = serde_json::to_vec_pretty(&result)?;
    ensure!(
        campaign.evidence_bytes + summary.len() as u64 <= campaign.limits.evidence_bytes,
        "reducer evidence budget exhausted"
    );
    campaign.evidence_bytes += summary.len() as u64;
    std::fs::write(directory.join("reduction.json"), summary)?;
    std::fs::write(
        directory.join("reduced-replay.json"),
        serde_json::to_vec_pretty(&result.reduced)?,
    )?;
    Ok(result)
}
