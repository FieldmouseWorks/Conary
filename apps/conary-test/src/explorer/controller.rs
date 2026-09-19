// apps/conary-test/src/explorer/controller.rs

use super::{checker::Oracle, contract::*, evidence::*, selector::Selector};
use anyhow::{Result, ensure};
use async_trait::async_trait;
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[async_trait]
pub trait Environment: Send {
    fn fixture_source(&self) -> Option<&std::path::Path> {
        None
    }
    async fn reset(&mut self) -> Result<Observation>;
    async fn verify(&self) -> Result<()>;
    async fn observe(&mut self) -> Result<Observation>;
    async fn execute(&mut self, operation_id: &str, action: &Action) -> Result<Receipt>;
    async fn close(&mut self) -> Result<()>;
}

#[derive(Debug, thiserror::Error)]
enum ControlStop {
    #[error("cancelled")]
    Cancelled,
    #[error("action budget exhausted")]
    Actions,
    #[error("wall-time budget exhausted (check/cleanup reserve)")]
    Time,
}

/// One allowance shared by initial run, replays, and reduction attempts.
pub struct Campaign {
    pub limits: Limits,
    pub attempted: u32,
    pub reductions: u32,
    pub evidence_bytes: u64,
    started: Instant,
}
impl Campaign {
    pub fn new(limits: Limits) -> Result<Self> {
        limits.validate()?;
        Ok(Self {
            limits,
            attempted: 0,
            reductions: 0,
            evidence_bytes: 0,
            started: Instant::now(),
        })
    }
    fn admit(&mut self, cancel: &AtomicBool) -> Result<()> {
        if cancel.load(Ordering::SeqCst) {
            return Err(ControlStop::Cancelled.into());
        }
        if self.attempted >= self.limits.actions {
            return Err(ControlStop::Actions.into());
        }
        if self.started.elapsed().as_secs()
            + self.limits.operation_seconds
            + self.limits.cleanup_seconds
            >= self.limits.seconds
        {
            return Err(ControlStop::Time.into());
        }
        self.attempted += 1;
        Ok(())
    }
    fn revalidate_dispatch(&self, cancel: &AtomicBool) -> Result<()> {
        if cancel.load(Ordering::SeqCst) {
            return Err(ControlStop::Cancelled.into());
        }
        if self.attempted > self.limits.actions {
            return Err(ControlStop::Actions.into());
        }
        // Reserve both an operation and the mandatory post-state check, plus cleanup.
        if self.started.elapsed().as_secs()
            + 2 * self.limits.operation_seconds
            + self.limits.cleanup_seconds
            >= self.limits.seconds
        {
            return Err(ControlStop::Time.into());
        }
        Ok(())
    }
    fn timeout(&self) -> Duration {
        Duration::from_secs(
            self.limits.operation_seconds.min(
                self.limits
                    .seconds
                    .saturating_sub(self.started.elapsed().as_secs())
                    .saturating_sub(self.limits.cleanup_seconds),
            ),
        )
    }
}

pub struct Episode<'a> {
    pub mode: Mode,
    pub identity: Identity,
    pub replay: Option<&'a Replay>,
    pub output: &'a std::path::Path,
    pub cancel: &'a AtomicBool,
}

/// Evaluation and cleanup run regardless of selector stop/error/cancellation.
pub async fn run(
    environment: &mut dyn Environment,
    mut selector: Option<&mut dyn Selector>,
    campaign: &mut Campaign,
    episode: Episode<'_>,
) -> Result<RunReport> {
    let remaining = campaign
        .limits
        .evidence_bytes
        .saturating_sub(campaign.evidence_bytes);
    ensure!(remaining >= 65536, "campaign evidence exhausted");
    let mut evidence = Evidence::create(episode.output, remaining)?;
    let mut oracle = Oracle::default();
    let mut report = RunReport {
        version: VERSION,
        mode: episode.mode,
        harness_revision: crate::build_info::BuildInfo::current().git_commit,
        selector_configuration: selector
            .as_ref()
            .map(|s| s.configuration())
            .unwrap_or(serde_json::Value::Null),
        selector: if episode.mode == Mode::Replay {
            "none-recorded-operations"
        } else {
            selector.as_ref().map(|s| s.identity()).unwrap_or("missing")
        }
        .into(),
        identity: episode.identity,
        limits: campaign.limits.clone(),
        operations: Vec::new(),
        evaluations: Vec::new(),
        stop_reason: "not_started".into(),
        cleanup: "pending".into(),
        reset_verified: false,
        final_publication: None,
        reproduces_recorded_predicate: None,
        attempted_actions_total: campaign.attempted,
        elapsed_ms: 0,
    };
    // Classify failures by the controller boundary that supplied the evidence.
    // Diagnostic strings and a nonzero CLI exit cannot establish product fault.
    let mut failure_class = Classification::HarnessFailure;
    let outcome: Result<()> = async {
        ensure!(
            episode.mode == Mode::Replay || episode.replay.is_none(),
            "replay supplied to a selector mode"
        );
        if let Some(replay) = episode.replay {
            ensure!(
                episode.mode == Mode::Replay && selector.is_none(),
                "replay must not have a selector"
            );
            ensure!(
                replay.version == VERSION && replay.operations.len() <= 64,
                "invalid replay version/length"
            );
            ensure!(
                digest(&replay.identity)? == digest(&report.identity)?,
                "replay build/image/fixture mismatch"
            );
        }
        if let Some(source) = environment.fixture_source() {
            evidence.include_fixtures(source, &report.identity.fixtures)?;
        }
        evidence.event("started", &report)?;
        failure_class = Classification::EnvironmentFailure;
        let baseline = tokio::time::timeout(campaign.timeout(), environment.reset()).await??;
        failure_class = Classification::HarnessFailure;
        let checks = oracle.evaluate(&baseline.facts, false);
        ensure!(
            checks.iter().all(|e| e.passed == Some(true)),
            "baseline restoration not verified"
        );
        ensure!(
            baseline.version == VERSION && baseline.revision == 0,
            "invalid baseline revision"
        );
        report.reset_verified = true;
        evidence.event("baseline", &baseline)?;
        let mut operation_ids = BTreeSet::new();
        loop {
            if episode
                .replay
                .is_some_and(|r| report.operations.len() == r.operations.len())
            {
                report.stop_reason = "recorded_trace_complete".into();
                break;
            }
            campaign.admit(episode.cancel)?;
            failure_class = Classification::Inconclusive;
            let observed =
                tokio::time::timeout(campaign.timeout(), environment.observe()).await??;
            failure_class = Classification::HarnessFailure;
            ensure!(
                observed.environment == baseline.environment && observed.epoch == baseline.epoch,
                "environment changed during episode"
            );
            let request =
                DecisionRequest::new(observed, campaign.limits.actions - campaign.attempted + 1)?;
            evidence.event("decision_request", &request)?;
            let decision = if let Some(replay) = episode.replay {
                let action = &replay.operations[report.operations.len()];
                let candidate = request
                    .candidates
                    .iter()
                    .find(|c| &c.action == action)
                    .ok_or_else(|| anyhow::anyhow!("replay precondition unavailable"))?;
                Decision {
                    candidate_id: candidate.id.clone(),
                    binding: request.binding.clone(),
                }
            } else {
                let selected = selector
                    .as_deref_mut()
                    .ok_or_else(|| anyhow::anyhow!("missing selector"))?;
                let result =
                    tokio::time::timeout(campaign.timeout(), selected.select(&request)).await;
                evidence.event("selector_receipts", &selected.take_evidence())?;
                if episode.cancel.load(Ordering::SeqCst) {
                    return Err(ControlStop::Cancelled.into());
                }
                result??
            };
            evidence.event("decision", &decision)?;
            if episode.cancel.load(Ordering::SeqCst) {
                return Err(ControlStop::Cancelled.into());
            }
            ensure!(
                campaign.started.elapsed().as_secs() + campaign.limits.cleanup_seconds
                    < campaign.limits.seconds,
                "deadline before dispatch"
            );
            failure_class = Classification::EnvironmentFailure;
            tokio::time::timeout(campaign.timeout(), environment.verify()).await??;
            failure_class = Classification::Inconclusive;
            let current = tokio::time::timeout(campaign.timeout(), environment.observe()).await??;
            failure_class = Classification::HarnessFailure;
            campaign.revalidate_dispatch(episode.cancel)?;
            let action = request.authorize(&decision, &current)?;
            let operation_id = format!(
                "{}:{}:{}",
                current.environment, current.epoch, current.revision
            );
            ensure!(
                operation_ids.insert(operation_id.clone()),
                "duplicate operation ID"
            );
            evidence.event("authorized_intent", &(&operation_id, &current, &action))?;
            if action == Action::Stop {
                report.operations.push(action);
                report.stop_reason = "selector_stop".into();
                break;
            }
            // Record concrete intent even if the receipt is lost: never silently retry it.
            campaign.revalidate_dispatch(episode.cancel)?;
            report.operations.push(action.clone());
            failure_class = Classification::Inconclusive;
            let receipt = tokio::time::timeout(
                campaign.timeout(),
                environment.execute(&operation_id, &action),
            )
            .await??;
            failure_class = Classification::HarnessFailure;
            ensure!(
                receipt.operation_id == operation_id && receipt.action == action,
                "receipt binding mismatch"
            );
            ensure!(
                receipt.stdout.len() + receipt.stderr.len() <= 65536,
                "operation output limit exceeded"
            );
            evidence.event("execution_receipt", &receipt)?;
            failure_class = Classification::Inconclusive;
            let after = tokio::time::timeout(campaign.timeout(), environment.observe()).await??;
            failure_class = Classification::HarnessFailure;
            let negative_request = oracle.expects_refusal(&action);
            if negative_request || receipt.exit_code != 0 {
                // The criterion is deliberately narrow: refusal with unchanged
                // independently observed fixture state. Diagnostic prose is not authority.
                let expected_refusal = negative_request && receipt.exit_code == 1
                    && current.facts.complete && after.facts.complete && current.facts == after.facts;
                let classification = if expected_refusal {
                    Classification::ExpectedRefusal
                } else if negative_request && receipt.exit_code == 0 {
                    Classification::ProductFailure
                } else {
                    Classification::Inconclusive
                };
                report.evaluations.push(Evaluation {
                    criterion: "operation.refusal_or_success".into(), checker: CHECKER.into(),
                    classification,
                    passed: (classification != Classification::Inconclusive).then_some(expected_refusal),
                    detail: format!("exit {}; required refusal checks unchanged fixture state; an unexplained nonzero exit has no established product/environment cause", receipt.exit_code),
                });
            }
            if !negative_request { oracle.accept(&receipt); }
            let checks = oracle.evaluate(&after.facts, action == Action::NegativeControl);
            evidence.event("checked", &(&after, &checks))?;
            report.evaluations.extend(checks);
        }
        Ok(())
    }
    .await;
    if let Err(error) = outcome {
        report.stop_reason = error.to_string();
        if error.downcast_ref::<ControlStop>().is_none()
            && error
                .downcast_ref::<super::selector::RequestBudgetExhausted>()
                .is_none()
        {
            report.evaluations.push(Evaluation {
                criterion: match failure_class {
                    Classification::EnvironmentFailure => "environment.preflight",
                    Classification::Inconclusive => "operation.evidence",
                    _ => "harness.execution",
                }
                .into(),
                checker: CHECKER.into(),
                classification: failure_class,
                passed: None,
                detail: format!("{error:#}"),
            });
        }
    }
    // This is unconditional, including Stop, failed guards and exhausted budgets.
    match tokio::time::timeout(campaign.timeout(), environment.observe()).await {
        Ok(Ok(observation)) if report.reset_verified => {
            report.final_publication = observation.facts.publication.clone();
            let checks = oracle.evaluate(&observation.facts, false);
            let _ = evidence.event("required_final_checks", &(&observation, &checks));
            report.evaluations.extend(checks);
        }
        _ => report.evaluations.push(Evaluation {
            criterion: "required.final_checks".into(),
            checker: CHECKER.into(),
            classification: Classification::Inconclusive,
            passed: None,
            detail: "final observation unavailable or baseline unverified".into(),
        }),
    }
    report.cleanup = match tokio::time::timeout(
        Duration::from_secs(campaign.limits.cleanup_seconds),
        environment.close(),
    )
    .await
    {
        Ok(Ok(())) => "removed".into(),
        Ok(Err(e)) => format!("failed; quarantine environment: {e:#}"),
        Err(_) => "timed out; quarantine environment".into(),
    };
    report.reproduces_recorded_predicate = episode.replay.map(|r| {
        report.reset_verified
            && report.cleanup == "removed"
            && matches!(
                report.stop_reason.as_str(),
                "recorded_trace_complete" | "selector_stop"
            )
            && report.evaluations.iter().all(|e| e.passed.is_some())
            && report.signatures() == r.failure_signatures
    });
    report.attempted_actions_total = campaign.attempted;
    report.elapsed_ms = campaign.started.elapsed().as_millis() as u64;
    let finished = evidence.finish(&report);
    campaign.evidence_bytes += evidence.bytes_written();
    finished?;
    Ok(report)
}
