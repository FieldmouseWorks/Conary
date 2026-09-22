// apps/conary-test/src/explorer/coverage.rs

//! Deterministic, consumer-owned coverage advice; observed facts remain authoritative.
use super::{
    context::{DecisionContext, POLICY},
    contract::*,
    selector::Selector,
};
use anyhow::{Result, ensure};
use async_trait::async_trait;
use serde_json::{Value, json};

pub struct CoverageGreedy;

#[async_trait]
impl Selector for CoverageGreedy {
    fn identity(&self) -> &'static str {
        "coverage-greedy-v1"
    }

    fn configuration(&self) -> Value {
        json!({
            "policy": self.identity(),
            "decision_policy": POLICY,
            "ranking": ["untried_from_current_state", "unvisited_projected_state",
                "fewest_attempts_from_current_state", "fewest_total_attempts", "candidate_order"],
            "projections_are_observations": false,
            "incomplete_observation": "stop"
        })
    }

    async fn select(
        &mut self,
        request: &DecisionRequest,
        context: &DecisionContext,
    ) -> Result<Decision> {
        ensure!(
            context.version == 1
                && context.policy == POLICY
                && context.request_binding == request.binding
                && context.candidates.len() == request.candidates.len()
                && request.candidates.iter().all(|candidate| context
                    .candidates
                    .get(&candidate.id)
                    .is_some_and(|advice| advice.action == candidate.action)),
            "decision context binding mismatch"
        );
        let current = &request.observation.facts.packages;
        let choice = request
            .candidates
            .iter()
            .enumerate()
            .filter_map(|(index, candidate)| {
                if !request.observation.facts.complete {
                    return None;
                }
                // Predict only the declared package effect. A refusal does not visit it.
                let mut projected = current.clone();
                match &candidate.action {
                    Action::Install(fixture) | Action::Update(fixture) => {
                        projected.insert(fixture.package(), fixture.version().into());
                    }
                    Action::Remove(package) => {
                        projected.remove(package);
                    }
                    _ => return None,
                }
                if projected == *current {
                    return None;
                }
                let advice = &context.candidates[&candidate.id];
                let visited = context
                    .coverage
                    .visited_states
                    .iter()
                    .any(|state| state.packages == projected);
                Some((
                    (
                        advice.attempts_from_current_state != 0,
                        visited,
                        advice.attempts_from_current_state,
                        advice.attempts_total,
                        index,
                    ),
                    candidate,
                ))
            })
            .min_by_key(|(rank, _)| *rank)
            .map(|(_, candidate)| candidate)
            .or_else(|| request.candidates.iter().find(|c| c.action == Action::Stop))
            .ok_or_else(|| anyhow::anyhow!("no state-changing or stop candidate"))?;
        Ok(Decision {
            candidate_id: choice.id.clone(),
            binding: request.binding.clone(),
        })
    }
}
