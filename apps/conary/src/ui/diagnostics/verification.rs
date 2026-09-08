// apps/conary/src/ui/diagnostics/verification.rs
//! Project original core facts into the versioned machine contract. Human
//! wording comes from the same diagnostic owner; displayed fields are never parsed.

use crate::commands::ccs::verification::VerificationInputError;
use conary_agent_contract::{
    CcsAuthorityDiagnostic, CcsAuthorityDiagnosticCode as Code,
    CcsAuthorityDiagnosticSeverity as Severity, CcsVerificationCause as Cause,
    CcsVerificationFailure, CcsVerificationReport,
};
use conary_core::ccs::v3::diagnostics::V3DiagnosticSeverity;
use conary_core::ccs::v3::{V3DiagnosticCode, V3ValidationError};
use conary_core::ccs::verify::{
    TrustPolicySubject, TrustViolation, VerificationSubject, VerifyError,
};

#[derive(Debug, thiserror::Error)]
#[error("CCS verification failed; JSON result written")]
pub(super) struct ReportedVerificationFailure;

pub(crate) fn write_verification_report(report: &CcsVerificationReport) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(report)?;
    crate::ui::message(&json);
    if report.is_verified() {
        Ok(())
    } else {
        Err(ReportedVerificationFailure.into())
    }
}

pub(crate) fn verification_failure(error: &anyhow::Error) -> CcsVerificationFailure {
    let diagnostic = super::from_error(error);
    CcsVerificationFailure {
        cause: cause(error),
        summary: diagnostic.message,
        notes: diagnostic.notes,
        archive_path: error
            .downcast_ref::<VerificationSubject>()
            .map(|subject| subject.path.to_string_lossy().into_owned()),
        policy_path: error
            .downcast_ref::<TrustPolicySubject>()
            .map(|subject| subject.path.to_string_lossy().into_owned()),
    }
}

fn cause(error: &anyhow::Error) -> Cause {
    if let Some(error) = error.downcast_ref::<VerifyError>() {
        return verification_cause(error);
    }
    if let Some(error) = error.downcast_ref::<V3ValidationError>() {
        return Cause::AuthorityInvalid {
            diagnostics: error
                .diagnostics
                .iter()
                .map(|diagnostic| CcsAuthorityDiagnostic {
                    code: match diagnostic.code {
                        V3DiagnosticCode::MissingAuthority => Code::MissingAuthority,
                        V3DiagnosticCode::UnsupportedFormatVersion => {
                            Code::UnsupportedFormatVersion
                        }
                        V3DiagnosticCode::TomlOnlyAuthority => Code::TomlOnlyAuthority,
                        V3DiagnosticCode::KindContractViolation => Code::KindContractViolation,
                        V3DiagnosticCode::ComponentAuthorityMismatch => {
                            Code::ComponentAuthorityMismatch
                        }
                        V3DiagnosticCode::IdentityUnstable => Code::IdentityUnstable,
                        V3DiagnosticCode::ConversionNotNative => Code::ConversionNotNative,
                        V3DiagnosticCode::StructuralBudgetExceeded => {
                            Code::StructuralBudgetExceeded
                        }
                    },
                    severity: match diagnostic.severity {
                        V3DiagnosticSeverity::Error => Severity::Error,
                        V3DiagnosticSeverity::Warning => Severity::Warning,
                        V3DiagnosticSeverity::Info => Severity::Info,
                    },
                    message: diagnostic.message.clone(),
                    field: diagnostic.field.clone(),
                    path: diagnostic.path.clone(),
                    invalid: diagnostic.invalid,
                    suggestion: diagnostic.suggestion.clone(),
                })
                .collect(),
        };
    }
    if let Some(error) = error.downcast_ref::<conary_core::ccs::BudgetError>() {
        return Cause::BudgetExceeded {
            dimension: error.dimension.to_string(),
            field: error.field.clone(),
            observed: error.observed,
            limit: error.limit,
        };
    }
    if let Some(error) = error.downcast_ref::<VerificationInputError>() {
        return match error {
            VerificationInputError::PackageNotFound { path } => {
                Cause::PackageNotFound { path: path.clone() }
            }
            VerificationInputError::MissingPolicy => Cause::MissingPolicy {},
        };
    }
    Cause::Unclassified {
        causes: error.chain().map(ToString::to_string).collect(),
    }
}

fn verification_cause(error: &VerifyError) -> Cause {
    match error {
        VerifyError::NotSigned => Cause::NotSigned {},
        VerifyError::InvalidSignatureFormat(detail) => Cause::InvalidSignatureFormat {
            detail: detail.clone(),
        },
        VerifyError::UnsupportedAlgorithm { algorithm } => Cause::UnsupportedAlgorithm {
            algorithm: algorithm.clone(),
        },
        VerifyError::SignatureInvalid(detail) => Cause::SignatureInvalid {
            detail: detail.clone(),
        },
        VerifyError::PayloadInvalid(detail) => Cause::PayloadInvalid {
            detail: detail.clone(),
        },
        VerifyError::PackageError(detail) => Cause::PackageInvalid {
            detail: detail.clone(),
        },
        VerifyError::TrustViolation(cause) => match cause {
            TrustViolation::NoTrustedKeys => Cause::NoTrustedKeys {},
            TrustViolation::DuplicateTrustedKey { public_key } => Cause::DuplicateTrustedKey {
                public_key: public_key.clone(),
            },
            TrustViolation::UntrustedSigner { key_id, public_key } => Cause::UntrustedSigner {
                claimed_key_id: key_id.clone(),
                public_key: public_key.clone(),
            },
            TrustViolation::MissingTimestamp => Cause::MissingTimestamp {},
            TrustViolation::InvalidTimestamp { timestamp } => Cause::InvalidTimestamp {
                timestamp: timestamp.clone(),
            },
            TrustViolation::FutureTimestamp { timestamp } => Cause::FutureTimestamp {
                timestamp: timestamp.clone(),
            },
            TrustViolation::ExpiredSignature {
                timestamp,
                age_seconds,
                max_age_seconds,
            } => Cause::ExpiredSignature {
                timestamp: timestamp.clone(),
                age_seconds: *age_seconds,
                max_age_seconds: *max_age_seconds,
            },
        },
    }
}

#[cfg(test)]
mod tests;
