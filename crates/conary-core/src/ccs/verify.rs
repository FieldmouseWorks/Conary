// crates/conary-core/src/ccs/verify.rs

//! Trusted CCS v3 package verification.
//!
//! `archive_reader` remains an explicitly untrusted diagnostic decoder. This
//! module authenticates metadata before streaming signed objects to a spool.

use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::path::Path;

mod archive_identity;
mod errors;
pub use errors::{TrustPolicySubject, TrustViolation, VerificationSubject, VerifyError};
pub(crate) mod content;
mod object_sink;
mod stream;

#[derive(Debug, Clone)]
pub(crate) struct RawControlDocuments {
    pub manifest: Vec<u8>,
    pub signature: String,
    pub debug_toml: Option<Vec<u8>>,
    pub build_attestation: Option<String>,
    pub foreign_conversion_boundary: Option<String>,
}

/// Signature data embedded in a current CCS package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageSignature {
    pub algorithm: String,
    pub signature: String,
    pub public_key: String,
    #[serde(default)]
    pub key_id: Option<String>,
    #[serde(default)]
    pub timestamp: Option<String>,
}

/// Exact trust anchors and timestamp constraints for CCS verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustPolicy {
    trusted_keys: Vec<String>,
    require_timestamp: bool,
    max_signature_age: u64,
}

impl TrustPolicy {
    /// Require a signature from one of the supplied Ed25519 public keys.
    pub fn strict(trusted_keys: Vec<String>) -> Self {
        Self {
            trusted_keys,
            require_timestamp: true,
            max_signature_age: 0,
        }
    }

    pub fn with_timestamp_required(mut self, required: bool) -> Self {
        self.require_timestamp = required;
        self
    }

    pub fn with_max_signature_age(mut self, seconds: u64) -> Self {
        self.max_signature_age = seconds;
        self
    }

    pub fn trusted_keys(&self) -> &[String] {
        &self.trusted_keys
    }

    pub fn from_file(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("read CCS trust policy {}", path.display()))?;
        Self::from_toml(&content).with_context(|| TrustPolicySubject {
            path: path.to_path_buf(),
        })
    }

    pub fn from_toml(content: &str) -> Result<Self> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct PolicyFile {
            trusted_keys: Vec<String>,
            #[serde(default = "default_true")]
            require_timestamp: bool,
            #[serde(default)]
            max_signature_age: u64,
        }

        fn default_true() -> bool {
            true
        }

        let parsed: PolicyFile = toml::from_str(content).context("parse CCS trust policy")?;
        let policy = Self {
            trusted_keys: parsed.trusted_keys,
            require_timestamp: parsed.require_timestamp,
            max_signature_age: parsed.max_signature_age,
        };
        policy.validate()?;
        Ok(policy)
    }

    fn validate(&self) -> Result<()> {
        if self.trusted_keys.is_empty() {
            return Err(VerifyError::TrustViolation(TrustViolation::NoTrustedKeys).into());
        }
        let mut unique = BTreeSet::new();
        for key in &self.trusted_keys {
            let bytes = BASE64.decode(key).map_err(|error| {
                VerifyError::InvalidSignatureFormat(format!(
                    "trusted public key is not base64: {error}"
                ))
            })?;
            let key_bytes: [u8; 32] = bytes.try_into().map_err(|bytes: Vec<u8>| {
                VerifyError::InvalidSignatureFormat(format!(
                    "trusted public key decoded to {} bytes; expected 32",
                    bytes.len()
                ))
            })?;
            VerifyingKey::from_bytes(&key_bytes).map_err(|error| {
                VerifyError::InvalidSignatureFormat(format!(
                    "trusted public key is not Ed25519: {error}"
                ))
            })?;
            if !unique.insert(key) {
                return Err(
                    VerifyError::TrustViolation(TrustViolation::DuplicateTrustedKey {
                        public_key: key.clone(),
                    })
                    .into(),
                );
            }
        }
        Ok(())
    }
}

/// Exact physical identity produced while streaming a complete CCS archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedArchiveIdentity {
    sha256: String,
    bytes: u64,
}

/// Exact bounded worker and block geometry used by authenticated archive decode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VerifiedArchiveDecodeMetrics {
    pub workers: u64,
    pub blocks: u64,
    pub decoded_bytes: u64,
    pub block_bytes: u64,
    pub buffer_ceiling_bytes: u64,
}

impl From<crate::ccs::archive_framing::ArchiveDecodeMetrics> for VerifiedArchiveDecodeMetrics {
    fn from(value: crate::ccs::archive_framing::ArchiveDecodeMetrics) -> Self {
        Self {
            workers: value.workers,
            blocks: value.blocks,
            decoded_bytes: value.decoded_bytes,
            block_bytes: value.block_bytes,
            buffer_ceiling_bytes: value.buffer_ceiling_bytes,
        }
    }
}

impl VerifiedArchiveIdentity {
    fn new(sha256: String, bytes: u64) -> Self {
        Self { sha256, bytes }
    }

    /// SHA-256 of every compressed byte in the exact verified archive.
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Exact compressed archive byte length covered by [`Self::sha256`].
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

/// Authenticated current CCS package capability.
///
/// Archive-backed verification carries an exact compressed archive identity.
/// A capability reconstructed from authenticated transport controls and CAS
/// objects does not claim that a compressed archive was read.
#[derive(Debug, Clone)]
pub struct VerifiedCcsArchive {
    archive_identity: Option<VerifiedArchiveIdentity>,
    archive_decode_metrics: Option<VerifiedArchiveDecodeMetrics>,
    authority: crate::ccs::v3::AuthorityDocumentV3,
    signature: PackageSignature,
    build_attestation: Option<crate::ccs::attestation::BuildAttestationEnvelope>,
    foreign_conversion_boundary: Option<crate::ccs::attestation::ForeignConversionBoundary>,
    debug_toml: Option<Vec<u8>>,
    components: HashMap<String, crate::ccs::builder::ComponentData>,
    payload: crate::packages::payload::PackagePayload,
    object_sources: HashMap<String, crate::packages::payload::ReopenablePayload>,
    verified_object_metrics: Option<crate::filesystem::VerifiedObjectBatchMetrics>,
    files_checked: usize,
    raw: RawControlDocuments,
}

impl VerifiedCcsArchive {
    /// Exact compressed identity when verification read an archive.
    ///
    /// Transport-created capabilities have no compressed archive identity and
    /// therefore return `None`.
    pub fn archive_identity(&self) -> Option<&VerifiedArchiveIdentity> {
        self.archive_identity.as_ref()
    }

    /// SHA-256 of every compressed byte when verification read an archive.
    pub fn archive_sha256(&self) -> Option<&str> {
        self.archive_identity().map(VerifiedArchiveIdentity::sha256)
    }

    /// Exact compressed byte length covered by [`Self::archive_sha256`].
    pub fn archive_bytes(&self) -> Option<u64> {
        self.archive_identity().map(VerifiedArchiveIdentity::bytes)
    }

    /// Exact parallel decode geometry when this capability came from an archive.
    pub fn archive_decode_metrics(&self) -> Option<VerifiedArchiveDecodeMetrics> {
        self.archive_decode_metrics
    }

    pub fn authority(&self) -> &crate::ccs::v3::AuthorityDocumentV3 {
        &self.authority
    }

    pub fn signature(&self) -> &PackageSignature {
        &self.signature
    }

    pub fn package_name(&self) -> &str {
        &self.authority.identity.name
    }

    pub fn package_version(&self) -> &str {
        &self.authority.identity.version
    }

    pub fn files_checked(&self) -> usize {
        self.files_checked
    }

    /// Permanent-CAS work performed by install-oriented verification.
    pub fn verified_object_metrics(&self) -> Option<crate::filesystem::VerifiedObjectBatchMetrics> {
        self.verified_object_metrics
    }

    pub(crate) fn object_sources(
        &self,
    ) -> &HashMap<String, crate::packages::payload::ReopenablePayload> {
        &self.object_sources
    }

    pub(crate) fn raw_control_documents(&self) -> &RawControlDocuments {
        &self.raw
    }

    pub(crate) fn build_attestation(
        &self,
    ) -> Option<&crate::ccs::attestation::BuildAttestationEnvelope> {
        self.build_attestation.as_ref()
    }

    pub(crate) fn foreign_conversion_boundary(
        &self,
    ) -> Option<&crate::ccs::attestation::ForeignConversionBoundary> {
        self.foreign_conversion_boundary.as_ref()
    }

    pub(crate) fn debug_toml(&self) -> Option<&[u8]> {
        self.debug_toml.as_deref()
    }

    pub(crate) fn components(&self) -> &HashMap<String, crate::ccs::builder::ComponentData> {
        &self.components
    }

    pub(crate) fn payload(&self) -> &crate::packages::payload::PackagePayload {
        &self.payload
    }
}

/// Verify current CCS authority, signature trust, diagnostic projections, and
/// every payload object before returning an install/publication capability.
pub fn verify_package(path: &Path, policy: &TrustPolicy) -> Result<VerifiedCcsArchive> {
    let admission = crate::ccs::CcsArchiveCpuAdmission::for_current_process();
    verify_package_with_archive_cpu_admission(path, policy, &admission)
}

/// Verify one archive with an explicit shared aggregate archive-CPU authority.
pub fn verify_package_with_archive_cpu_admission(
    path: &Path,
    policy: &TrustPolicy,
    admission: &crate::ccs::CcsArchiveCpuAdmission,
) -> Result<VerifiedCcsArchive> {
    verify_package_to(
        path,
        policy,
        object_sink::ObjectDestination::Spool,
        admission,
    )
}

/// Verify a current CCS archive directly into one permanent SHA-256 CAS.
///
/// The returned payload sources carry the committed batch capability, allowing
/// installation to reference their canonical identities without reopening and
/// re-ingesting the same bytes.
pub fn verify_package_into_cas(
    path: &Path,
    policy: &TrustPolicy,
    cas: &crate::filesystem::CasStore,
) -> Result<VerifiedCcsArchive> {
    let admission = crate::ccs::CcsArchiveCpuAdmission::for_current_process();
    verify_package_into_cas_with_archive_cpu_admission(path, policy, cas, &admission)
}

/// Verify directly into permanent CAS under one shared archive-CPU authority.
pub fn verify_package_into_cas_with_archive_cpu_admission(
    path: &Path,
    policy: &TrustPolicy,
    cas: &crate::filesystem::CasStore,
    admission: &crate::ccs::CcsArchiveCpuAdmission,
) -> Result<VerifiedCcsArchive> {
    verify_package_to(
        path,
        policy,
        object_sink::ObjectDestination::Permanent(cas),
        admission,
    )
}

fn verify_package_to(
    path: &Path,
    policy: &TrustPolicy,
    destination: object_sink::ObjectDestination<'_>,
    admission: &crate::ccs::CcsArchiveCpuAdmission,
) -> Result<VerifiedCcsArchive> {
    let verified = (|| {
        policy.validate()?;
        let lease = admission.acquire()?;
        stream::verify_archive(path, policy, destination, lease.workers())
    })()
    .with_context(|| VerificationSubject {
        path: path.to_path_buf(),
    })?;
    Ok(VerifiedCcsArchive {
        archive_identity: Some(verified.archive_identity),
        archive_decode_metrics: Some(verified.archive_decode_metrics.into()),
        authority: verified.authority,
        signature: verified.signature,
        build_attestation: verified.build_attestation,
        foreign_conversion_boundary: verified.foreign_conversion_boundary,
        debug_toml: verified.debug_toml,
        components: verified.components,
        payload: verified.payload,
        object_sources: verified.object_sources,
        verified_object_metrics: verified.verified_object_metrics,
        files_checked: verified.files_checked,
        raw: verified.raw,
    })
}

/// Verify one manifest signature against exact archived bytes.
pub(crate) fn verify_manifest_signature(
    manifest_raw: &[u8],
    signature: &PackageSignature,
    policy: &TrustPolicy,
) -> Result<()> {
    policy.validate()?;
    if signature.algorithm != "ed25519" {
        return Err(VerifyError::UnsupportedAlgorithm {
            algorithm: signature.algorithm.clone(),
        }
        .into());
    }
    let sig_bytes = BASE64.decode(&signature.signature).map_err(|error| {
        VerifyError::InvalidSignatureFormat(format!("signature is not base64: {error}"))
    })?;
    let signature_bytes = Signature::from_slice(&sig_bytes).map_err(|error| {
        VerifyError::InvalidSignatureFormat(format!("invalid Ed25519 signature bytes: {error}"))
    })?;
    let key_bytes = BASE64.decode(&signature.public_key).map_err(|error| {
        VerifyError::InvalidSignatureFormat(format!("public key is not base64: {error}"))
    })?;
    let key_bytes: [u8; 32] = key_bytes.try_into().map_err(|bytes: Vec<u8>| {
        VerifyError::InvalidSignatureFormat(format!(
            "public key decoded to {} bytes; expected 32",
            bytes.len()
        ))
    })?;
    let verifying_key = VerifyingKey::from_bytes(&key_bytes).map_err(|error| {
        VerifyError::InvalidSignatureFormat(format!("invalid Ed25519 public key: {error}"))
    })?;
    verifying_key
        .verify_strict(manifest_raw, &signature_bytes)
        .map_err(|error| VerifyError::SignatureInvalid(error.to_string()))?;

    if !policy.trusted_keys.contains(&signature.public_key) {
        return Err(
            VerifyError::TrustViolation(TrustViolation::UntrustedSigner {
                key_id: signature.key_id.clone(),
                public_key: signature.public_key.clone(),
            })
            .into(),
        );
    }
    if policy.require_timestamp && signature.timestamp.is_none() {
        return Err(VerifyError::TrustViolation(TrustViolation::MissingTimestamp).into());
    }
    if let Some(timestamp) = &signature.timestamp {
        let signed_time = chrono::DateTime::parse_from_rfc3339(timestamp).map_err(|_| {
            VerifyError::TrustViolation(TrustViolation::InvalidTimestamp {
                timestamp: timestamp.clone(),
            })
        })?;
        if policy.max_signature_age > 0 {
            let age = chrono::Utc::now().signed_duration_since(signed_time);
            if age.num_seconds() < 0 {
                return Err(
                    VerifyError::TrustViolation(TrustViolation::FutureTimestamp {
                        timestamp: timestamp.clone(),
                    })
                    .into(),
                );
            }
            if age.num_seconds().unsigned_abs() > policy.max_signature_age {
                return Err(
                    VerifyError::TrustViolation(TrustViolation::ExpiredSignature {
                        timestamp: timestamp.clone(),
                        age_seconds: age.num_seconds().unsigned_abs(),
                        max_age_seconds: policy.max_signature_age,
                    })
                    .into(),
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
