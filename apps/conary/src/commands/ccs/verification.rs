// apps/conary/src/commands/ccs/verification.rs
//! Quiet verification service shared by human, JSON, and local MCP adapters.

use anyhow::{Context, Result};
use conary_agent_contract::{
    CcsVerificationOutcome, CcsVerificationReport, CcsVerificationSchema, VerifiedCcsFacts,
};
use conary_core::ccs::{TrustPolicy, VerifiedCcsArchive, verify};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub(crate) enum VerificationInputError {
    #[error("Package not found: {path}")]
    PackageNotFound { path: String },
    #[error("CCS verification requires --policy or an initialized local-dev signing key")]
    MissingPolicy,
}

fn verify_input(package: &str, policy_path: Option<&str>) -> Result<VerifiedCcsArchive> {
    let path = Path::new(package);
    if !path
        .try_exists()
        .with_context(|| format!("inspect CCS package {package}"))?
    {
        return Err(VerificationInputError::PackageNotFound {
            path: package.into(),
        }
        .into());
    }
    let policy = if let Some(policy_file) = policy_path {
        TrustPolicy::from_file(Path::new(policy_file)).context("Failed to load trust policy")?
    } else if let Some(local_policy) = super::local_dev::local_dev_trust_policy()? {
        local_policy
    } else {
        return Err(VerificationInputError::MissingPolicy.into());
    };
    verify::verify_package(path, &policy).context("Verification failed")
}

fn verified_facts(archive: &VerifiedCcsArchive) -> Result<VerifiedCcsFacts> {
    let identity = archive
        .archive_identity()
        .context("file verification did not retain its archive identity")?;
    Ok(VerifiedCcsFacts {
        name: archive.package_name().into(),
        version: archive.package_version().into(),
        architecture: archive.authority().identity.architecture.clone(),
        release: archive.authority().identity.release.clone(),
        version_scheme: archive.authority().identity.version_scheme.as_str().into(),
        archive_sha256: identity.sha256().into(),
        archive_bytes: identity.bytes(),
        files_checked: archive.files_checked() as u64,
        public_key: archive.signature().public_key.clone(),
        claimed_key_id: archive.signature().key_id.clone(),
        timestamp: archive.signature().timestamp.clone(),
    })
}

pub(crate) fn verification_report(package: &str, policy: Option<&str>) -> CcsVerificationReport {
    let result = verify_input(package, policy).and_then(|archive| verified_facts(&archive));
    CcsVerificationReport {
        schema: CcsVerificationSchema::V1,
        package: package.into(),
        policy: policy.map(str::to_owned),
        outcome: match result {
            Ok(facts) => CcsVerificationOutcome::Verified { facts },
            Err(error) => CcsVerificationOutcome::Failed {
                failure: crate::ui::diagnostics::verification_failure(&error),
            },
        },
    }
}

pub fn cmd_ccs_verify(package: &str, policy_path: Option<String>, json: bool) -> Result<()> {
    if json {
        let report = verification_report(package, policy_path.as_deref());
        return crate::ui::diagnostics::write_verification_report(&report);
    }
    let result = verify_input(package, policy_path.as_deref())?;
    let facts = verified_facts(&result)?;
    crate::ui::field("Package", package);
    crate::ui::row(
        crate::ui::Status::Ok,
        &[&format!("{} v{}", facts.name, facts.version)],
    );
    let signature = facts
        .claimed_key_id
        .as_deref()
        .map(|id| format!("Signature: valid key={id}"))
        .unwrap_or_else(|| "Signature: valid".into());
    crate::ui::row(crate::ui::Status::Ok, &[&signature]);
    crate::ui::row(
        crate::ui::Status::Ok,
        &[&format!("Content: {} files verified", facts.files_checked)],
    );
    Ok(())
}
