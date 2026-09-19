// apps/conary-test/src/explorer/evidence.rs

use super::contract::*;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub source_revision: String,
    pub conary_sha256: String,
    pub image: String,
    pub fixtures: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Replay {
    pub version: u32,
    pub identity: Identity,
    pub operations: Vec<Action>,
    pub failure_signatures: Vec<String>,
}
impl Replay {
    pub fn load(path: &Path) -> Result<Self> {
        ensure!(
            path.metadata()?.len() <= 1024 * 1024,
            "replay exceeds size limit"
        );
        let value: Self = serde_json::from_slice(&std::fs::read(path)?)?;
        ensure!(
            value.version == VERSION && value.operations.len() <= 64,
            "unsupported replay or action count"
        );
        Ok(value)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunReport {
    pub version: u32,
    pub mode: Mode,
    pub selector: String,
    pub identity: Identity,
    pub limits: Limits,
    pub operations: Vec<Action>,
    pub evaluations: Vec<Evaluation>,
    pub stop_reason: String,
    pub cleanup: String,
    pub reset_verified: bool,
    pub attempted_actions_total: u32,
    pub elapsed_ms: u64,
}
impl RunReport {
    pub fn signatures(&self) -> Vec<String> {
        let mut signatures = self
            .evaluations
            .iter()
            .filter(|e| e.passed == Some(false))
            .map(|e| format!("{}:{}:{:?}", e.checker, e.criterion, e.classification))
            .collect::<Vec<_>>();
        signatures.sort();
        signatures.dedup();
        signatures
    }
    pub fn replay(&self) -> Replay {
        Replay {
            version: VERSION,
            identity: self.identity.clone(),
            operations: self.operations.clone(),
            failure_signatures: self.signatures(),
        }
    }
}

/// Append and flush intent before dispatch. Never overwrite a previous run.
pub struct Evidence {
    directory: PathBuf,
    events: File,
    bytes: u64,
    limit: u64,
    sequence: u64,
}
impl Evidence {
    pub fn create(directory: &Path, limit: u64) -> Result<Self> {
        std::fs::create_dir(directory)?;
        let events = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(directory.join("events.jsonl"))?;
        Ok(Self {
            directory: directory.into(),
            events,
            bytes: 0,
            limit,
            sequence: 0,
        })
    }
    pub fn include_fixtures(
        &mut self,
        source: &Path,
        expected: &std::collections::BTreeMap<String, String>,
    ) -> Result<()> {
        ensure!(
            super::fixtures::hashes(source)? == *expected,
            "bundle fixture identity mismatch"
        );
        std::fs::create_dir(self.directory.join("fixtures"))?;
        for name in super::fixtures::MEMBERS {
            let bytes = std::fs::read(source.join(name))?;
            ensure!(
                self.bytes + bytes.len() as u64 + 65536 <= self.limit,
                "fixture evidence budget exhausted"
            );
            // Verify the exact bytes copied, including races after initial inspection.
            use sha2::Digest;
            ensure!(
                expected.get(name) == Some(&hex::encode(sha2::Sha256::digest(&bytes))),
                "fixture changed during copy"
            );
            std::fs::write(self.directory.join("fixtures").join(name), &bytes)?;
            self.bytes += bytes.len() as u64;
        }
        Ok(())
    }
    pub fn event(&mut self, kind: &str, data: &impl Serialize) -> Result<()> {
        let mut bytes = serde_json::to_vec(&serde_json::json!({
            "version": VERSION, "sequence": self.sequence,
            "at": chrono::Utc::now().to_rfc3339(), "event": kind, "data": data,
        }))?;
        bytes.push(b'\n');
        // Reserve bounded room for the final report and cleanup receipt.
        ensure!(
            self.bytes + bytes.len() as u64 <= self.limit.saturating_sub(32768),
            "evidence budget exhausted"
        );
        self.events.write_all(&bytes)?;
        self.events.sync_data()?;
        self.bytes += bytes.len() as u64;
        self.sequence += 1;
        Ok(())
    }
    pub fn bytes_written(&self) -> u64 {
        self.bytes
    }
    pub fn finish(&mut self, report: &RunReport) -> Result<()> {
        let replay = report.replay();
        let summary = format!(
            "# Conary explorer\n\nMode: {:?}; selector: {}\n\nStop: {}\n\nBaseline verified: {}; cleanup: {}\n\nAttempted actions (campaign): {}\n\nFailure predicates: {:?}\n\nFull choices, intent, receipts and independent checks: events.jsonl\nReplay input: replay.json (concrete operations; no selector).\n\nThis fixture corpus does not reproduce #917. Negative controls are checker calibration only.\n",
            report.mode,
            report.selector,
            report.stop_reason,
            report.reset_verified,
            report.cleanup,
            report.attempted_actions_total,
            report.signatures()
        );
        let files = [
            ("report.json", serde_json::to_vec_pretty(report)?),
            ("replay.json", serde_json::to_vec_pretty(&replay)?),
            ("report.md", summary.into_bytes()),
        ];
        let total = files.iter().map(|(_, b)| b.len() as u64).sum::<u64>();
        ensure!(
            self.bytes + total <= self.limit,
            "final evidence exceeds budget"
        );
        for (name, bytes) in files {
            std::fs::write(self.directory.join(name), bytes)?;
        }
        let mut manifest = std::collections::BTreeMap::new();
        for name in ["events.jsonl", "report.json", "replay.json", "report.md"] {
            let bytes = std::fs::read(self.directory.join(name))?;
            use sha2::Digest;
            manifest.insert(name, serde_json::json!({"sha256": hex::encode(sha2::Sha256::digest(&bytes)), "bytes": bytes.len()}));
        }
        for name in super::fixtures::MEMBERS {
            let path = self.directory.join("fixtures").join(name);
            if path.exists() {
                let bytes = std::fs::read(path)?;
                use sha2::Digest;
                manifest.insert(format!("fixtures/{name}"), serde_json::json!({"sha256": hex::encode(sha2::Sha256::digest(&bytes)), "bytes": bytes.len()}));
            }
        }
        let bytes = serde_json::to_vec_pretty(&manifest)?;
        ensure!(
            self.bytes + total + bytes.len() as u64 <= self.limit,
            "manifest exceeds evidence budget"
        );
        self.bytes += total + bytes.len() as u64;
        std::fs::write(self.directory.join("artifacts.json"), bytes)?;
        Ok(())
    }
}
