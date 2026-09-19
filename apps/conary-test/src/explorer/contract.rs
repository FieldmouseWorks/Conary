// apps/conary-test/src/explorer/contract.rs

//! Versioned experiment records. These confer no execution authority.
use std::collections::BTreeMap;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const VERSION: u32 = 1;
pub const CHECKER: &str = "fixture-payload-ownership-v1";

pub fn digest(value: &impl Serialize) -> Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(value)?)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Calibration,
    Exploration,
    Replay,
}

/// The entire action vocabulary; neither replay nor a selector can add argv.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "fixture",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Action {
    Install(Fixture),
    Remove(Package),
    Inspect,
    Check,
    NegativeControl,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Package {
    App,
    Companion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fixture {
    AppV1,
    AppV2,
    Companion,
}

impl Fixture {
    pub fn package(self) -> Package {
        match self {
            Self::AppV1 | Self::AppV2 => Package::App,
            Self::Companion => Package::Companion,
        }
    }
    pub fn version(self) -> &'static str {
        match self {
            Self::AppV1 | Self::Companion => "1.0.0",
            Self::AppV2 => "2.0.0",
        }
    }
    pub fn filename(self) -> &'static str {
        match self {
            Self::AppV1 => "app-v1.ccs",
            Self::AppV2 => "app-v2.ccs",
            Self::Companion => "companion.ccs",
        }
    }
    pub fn payload(self) -> &'static str {
        match self {
            Self::AppV1 => "redshirt app v1\n",
            Self::AppV2 => "redshirt app v2\n",
            Self::Companion => "redshirt companion\n",
        }
    }
}
impl Package {
    pub fn name(self) -> &'static str {
        match self {
            Self::App => "redshirt-app",
            Self::Companion => "redshirt-companion",
        }
    }
    pub fn path(self) -> String {
        format!("/usr/share/redshirt/{}", self.name())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Facts {
    /// Independently queried SQLite package versions.
    pub packages: BTreeMap<Package, String>,
    /// Independently queried SQLite file owners.
    pub owners: BTreeMap<Package, String>,
    /// SHA-256 of actual deployed bytes; absent files have no entry.
    pub payloads: BTreeMap<Package, String>,
    /// Missing observations must never become a passing empty state.
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub version: u32,
    pub environment: String,
    pub epoch: u64,
    pub revision: u64,
    pub facts: Facts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub id: String,
    pub action: Action,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionRequest {
    pub version: u32,
    pub observation: Observation,
    pub candidates: Vec<Candidate>,
    pub binding: String,
    pub remaining_actions: u32,
}

impl DecisionRequest {
    pub fn new(observation: Observation, remaining_actions: u32) -> Result<Self> {
        ensure!(
            observation.version == VERSION,
            "unsupported observation version"
        );
        let mut actions = vec![Action::Inspect, Action::Check, Action::Stop];
        if observation.facts.complete {
            for fixture in [Fixture::AppV1, Fixture::AppV2, Fixture::Companion] {
                if observation
                    .facts
                    .packages
                    .get(&fixture.package())
                    .map(String::as_str)
                    != Some(fixture.version())
                {
                    actions.push(Action::Install(fixture));
                }
            }
            for package in [Package::App, Package::Companion] {
                actions.push(Action::Remove(package));
            }
            if observation.facts.packages.contains_key(&Package::App) {
                actions.push(Action::NegativeControl);
            }
        }
        let candidates = actions
            .into_iter()
            .enumerate()
            .map(|(i, action)| Candidate {
                id: format!("c{i}"),
                action,
            })
            .collect::<Vec<_>>();
        let binding = digest(&(VERSION, &observation, &candidates, remaining_actions))?;
        Ok(Self {
            version: VERSION,
            observation,
            candidates,
            binding,
            remaining_actions,
        })
    }

    pub fn authorize(&self, decision: &Decision, current: &Observation) -> Result<Action> {
        ensure!(
            self.version == VERSION && current.version == VERSION,
            "unsupported contract"
        );
        ensure!(
            self.observation == *current,
            "stale observation or wrong environment/epoch"
        );
        let rebuilt = Self::new(current.clone(), self.remaining_actions)?;
        ensure!(
            *self == rebuilt && decision.binding == self.binding,
            "candidate binding mismatch"
        );
        ensure!(self.remaining_actions > 0, "action budget exhausted");
        self.candidates
            .iter()
            .find(|c| c.id == decision.candidate_id)
            .map(|c| c.action.clone())
            .ok_or_else(|| anyhow::anyhow!("unknown candidate ID"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    pub candidate_id: String,
    pub binding: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Classification {
    Pass,
    ExpectedRefusal,
    ProductFailure,
    KnownDefect,
    NegativeControl,
    Inconclusive,
    HarnessFailure,
    EnvironmentFailure,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evaluation {
    pub criterion: String,
    pub checker: String,
    pub classification: Classification,
    pub passed: Option<bool>,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub operation_id: String,
    pub action: Action,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub actions: u32,
    pub seconds: u64,
    pub operation_seconds: u64,
    pub cleanup_seconds: u64,
    pub evidence_bytes: u64,
    pub reductions: u32,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            actions: 64,
            seconds: 900,
            operation_seconds: 30,
            cleanup_seconds: 20,
            evidence_bytes: 8 * 1024 * 1024,
            reductions: 6,
        }
    }
}
impl Limits {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=64).contains(&self.actions)
                && (1..=900).contains(&self.seconds)
                && (1..=30).contains(&self.operation_seconds)
                && (1..=20).contains(&self.cleanup_seconds)
                && (65536..=8 * 1024 * 1024).contains(&self.evidence_bytes)
                && self.reductions <= 6,
            "limits exceed first-slice bounds"
        );
        Ok(())
    }
}
