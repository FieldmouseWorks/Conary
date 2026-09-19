// apps/conary-test/src/explorer/sandbox.rs

//! Local controller registration. This file is never supplied to a selector.
use super::contract::VERSION;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestApproval {
    pub version: u32,
    pub approved_disposable: bool,
    pub boot_id: String,
    pub image: String,
    pub source_revision: String,
    pub conary_sha256: String,
}
impl GuestApproval {
    pub fn load(path: &Path) -> Result<Self> {
        ensure!(path.metadata()?.len() <= 8192, "approval file too large");
        let approval: Self = serde_json::from_slice(&std::fs::read(path)?)?;
        approval.validate()?;
        Ok(approval)
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == VERSION && self.approved_disposable,
            "disposable guest approval required"
        );
        ensure!(
            self.image
                .strip_prefix("sha256:")
                .is_some_and(|s| valid_hex(s, 64)),
            "image must be a local immutable SHA-256 identity"
        );
        ensure!(
            valid_hex(&self.source_revision, 40) && valid_hex(&self.conary_sha256, 64),
            "exact source and binary identities required"
        );
        ensure!(
            self.boot_id.len() == 36
                && self
                    .boot_id
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() || c == '-'),
            "invalid boot identity"
        );
        Ok(())
    }
    pub fn verify_here(&self) -> Result<()> {
        self.validate()?;
        ensure!(
            std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?.trim() == self.boot_id,
            "approval belongs to another guest boot"
        );
        let result = std::process::Command::new("systemd-detect-virt")
            .arg("--vm")
            .output()?;
        ensure!(
            result.status.success(),
            "package experiments require a registered disposable VM"
        );
        // Runtime endpoint overrides could escape the approved guest.
        for name in [
            "DOCKER_HOST",
            "CONTAINER_HOST",
            "CONTAINER_CONNECTION",
            "DOCKER_CONTEXT",
        ] {
            ensure!(
                std::env::var_os(name).is_none(),
                "container endpoint overrides forbidden in explorer"
            );
        }
        Ok(())
    }
}
fn valid_hex(value: &str, len: usize) -> bool {
    value.len() == len && value.bytes().all(|c| c.is_ascii_hexdigit())
}
