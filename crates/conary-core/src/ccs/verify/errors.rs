// crates/conary-core/src/ccs/verify/errors.rs
//! Verification refusal facts. These errors never confer archive authority.

use std::path::PathBuf;
use thiserror::Error;

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum VerifyError {
    #[error("CCS v3 package is not signed")]
    NotSigned,
    #[error("invalid CCS v3 signature format: {0}")]
    InvalidSignatureFormat(String),
    #[error("unsupported CCS signature algorithm: {algorithm}")]
    UnsupportedAlgorithm { algorithm: String },
    #[error("CCS v3 signature verification failed: {0}")]
    SignatureInvalid(String),
    #[error(transparent)]
    TrustViolation(#[from] TrustViolation),
    #[error("CCS v3 payload authority failed: {0}")]
    PayloadInvalid(String),
    #[error("CCS v3 package structure failed: {0}")]
    PackageError(String),
}

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum TrustViolation {
    #[error("no trusted CCS package signing keys are configured")]
    NoTrustedKeys,
    #[error("trusted CCS package key set contains a duplicate key: {public_key}")]
    DuplicateTrustedKey { public_key: String },
    #[error("CCS v3 package signer is not trusted: public key {public_key}")]
    UntrustedSigner {
        // Package-provided labels are claims, never trust anchors.
        key_id: Option<String>,
        public_key: String,
    },
    #[error("signature timestamp is required")]
    MissingTimestamp,
    #[error("signature timestamp is malformed: {timestamp}")]
    InvalidTimestamp { timestamp: String },
    #[error("signature timestamp is in the future: {timestamp}")]
    FutureTimestamp { timestamp: String },
    #[error("signature is {age_seconds} seconds old; maximum is {max_age_seconds}")]
    ExpiredSignature {
        timestamp: String,
        age_seconds: u64,
        max_age_seconds: u64,
    },
}

/// Inspectable context on file-based verification, including policy failures.
/// The path is the requested input, not an authenticated package identity.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
#[error("verify CCS package {}", path.display())]
pub struct VerificationSubject {
    pub path: PathBuf,
}

/// The policy file supplied to the trust-policy reader.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
#[error("read CCS trust policy {}", path.display())]
pub struct TrustPolicySubject {
    pub path: PathBuf,
}
