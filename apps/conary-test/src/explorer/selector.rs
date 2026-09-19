// apps/conary-test/src/explorer/selector.rs

use super::contract::*;
use anyhow::{Result, ensure};
use async_trait::async_trait;
use std::collections::BTreeMap;

#[async_trait]
pub trait Selector: Send {
    fn take_evidence(&mut self) -> Vec<serde_json::Value> {
        Vec::new()
    }
    fn identity(&self) -> &'static str;
    async fn select(&mut self, request: &DecisionRequest) -> Result<Decision>;
}

/// Visit least-used transitions first, with a reproducible seeded tie-break.
/// Availability is recomputed from observations after each action.
pub struct Seeded {
    state: u64,
    visits: BTreeMap<Action, u32>,
}
impl Seeded {
    pub fn new(seed: u64) -> Self {
        Self {
            state: seed,
            visits: BTreeMap::new(),
        }
    }
}
#[async_trait]
impl Selector for Seeded {
    fn identity(&self) -> &'static str {
        "seeded-v1"
    }
    async fn select(&mut self, request: &DecisionRequest) -> Result<Decision> {
        let available = request
            .candidates
            .iter()
            .filter(|c| c.action != Action::Stop)
            .collect::<Vec<_>>();
        ensure!(!available.is_empty(), "empty candidate set");
        let least = available
            .iter()
            .map(|c| self.visits.get(&c.action).copied().unwrap_or(0))
            .min()
            .unwrap_or(0);
        let choices = available
            .into_iter()
            .filter(|c| self.visits.get(&c.action).copied().unwrap_or(0) == least)
            .collect::<Vec<_>>();
        self.state = self
            .state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let choice = choices[(self.state % choices.len() as u64) as usize];
        *self.visits.entry(choice.action.clone()).or_default() += 1;
        Ok(Decision {
            candidate_id: choice.id.clone(),
            binding: request.binding.clone(),
        })
    }
}

pub struct Scripted(pub Vec<Action>);
#[async_trait]
impl Selector for Scripted {
    fn identity(&self) -> &'static str {
        "scripted-calibration-v1"
    }
    async fn select(&mut self, request: &DecisionRequest) -> Result<Decision> {
        let action = if self.0.is_empty() {
            Action::Stop
        } else {
            self.0.remove(0)
        };
        let candidate = request
            .candidates
            .iter()
            .find(|c| c.action == action)
            .ok_or_else(|| anyhow::anyhow!("scripted action unavailable: {action:?}"))?;
        Ok(Decision {
            candidate_id: candidate.id.clone(),
            binding: request.binding.clone(),
        })
    }
}

pub fn calibration() -> Vec<Action> {
    vec![
        Action::Install(Fixture::Companion),
        Action::Install(Fixture::AppV1),
        Action::Inspect,
        Action::Update(Fixture::AppV2),
        Action::Check,
        Action::NegativeControl,
        Action::Remove(Package::App),
        Action::Check,
        Action::Stop,
    ]
}
