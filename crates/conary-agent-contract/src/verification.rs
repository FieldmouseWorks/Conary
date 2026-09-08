// crates/conary-agent-contract/src/verification.rs
//! Transport-neutral CCS verification reports. Reports are observations, never
//! install/publication capabilities or substitutes for core verification.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CcsVerificationRequest {
    pub package: String,
    /// An explicit policy keeps the agent operation independent of local-dev
    /// key initialization or public-key mirror maintenance.
    pub policy: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum CcsVerificationSchema {
    #[serde(rename = "conary.ccs.verification.v1")]
    V1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CcsVerificationReport {
    pub schema: CcsVerificationSchema,
    /// Requested input, not authenticated package identity.
    pub package: String,
    /// None identifies the CLI's existing local-dev policy selection.
    pub policy: Option<String>,
    pub outcome: CcsVerificationOutcome,
}

impl CcsVerificationReport {
    pub fn is_verified(&self) -> bool {
        matches!(self.outcome, CcsVerificationOutcome::Verified { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum CcsVerificationOutcome {
    Verified { facts: VerifiedCcsFacts },
    Failed { failure: CcsVerificationFailure },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VerifiedCcsFacts {
    pub name: String,
    pub version: String,
    pub architecture: Option<String>,
    pub release: String,
    pub version_scheme: String,
    pub archive_sha256: String,
    pub archive_bytes: u64,
    pub files_checked: u64,
    pub public_key: String,
    /// Package-supplied metadata; key membership is established by public_key.
    pub claimed_key_id: Option<String>,
    pub timestamp: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CcsVerificationFailure {
    pub cause: CcsVerificationCause,
    pub summary: String,
    pub notes: Vec<String>,
    pub archive_path: Option<String>,
    pub policy_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CcsVerificationCause {
    NotSigned {},
    InvalidSignatureFormat {
        detail: String,
    },
    UnsupportedAlgorithm {
        algorithm: String,
    },
    SignatureInvalid {
        detail: String,
    },
    NoTrustedKeys {},
    DuplicateTrustedKey {
        public_key: String,
    },
    UntrustedSigner {
        claimed_key_id: Option<String>,
        public_key: String,
    },
    MissingTimestamp {},
    InvalidTimestamp {
        timestamp: String,
    },
    FutureTimestamp {
        timestamp: String,
    },
    ExpiredSignature {
        timestamp: String,
        age_seconds: u64,
        max_age_seconds: u64,
    },
    PayloadInvalid {
        detail: String,
    },
    PackageInvalid {
        detail: String,
    },
    AuthorityInvalid {
        diagnostics: Vec<CcsAuthorityDiagnostic>,
    },
    BudgetExceeded {
        dimension: String,
        field: String,
        observed: u64,
        limit: u64,
    },
    MissingPolicy {},
    PackageNotFound {
        path: String,
    },
    /// Unclassified failures retain their actual context chain; adapters may
    /// not infer a typed cause or remediation from these strings.
    Unclassified {
        causes: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CcsAuthorityDiagnostic {
    pub code: CcsAuthorityDiagnosticCode,
    pub severity: CcsAuthorityDiagnosticSeverity,
    pub message: String,
    pub field: Option<String>,
    pub path: Option<String>,
    pub invalid: bool,
    pub suggestion: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum CcsAuthorityDiagnosticCode {
    MissingAuthority,
    UnsupportedFormatVersion,
    TomlOnlyAuthority,
    KindContractViolation,
    ComponentAuthorityMismatch,
    IdentityUnstable,
    ConversionNotNative,
    StructuralBudgetExceeded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum CcsAuthorityDiagnosticSeverity {
    Error,
    Warning,
    Info,
}

#[cfg(test)]
mod tests;
