// apps/conary/src/ui/diagnostics/ccs.rs
//! Present core-owned package verification and host-capability refusal facts.

use super::Diagnostic;
use conary_core::ccs::HostCapabilityPreflightError;
use conary_core::ccs::v3::{V3DiagnosticCode, V3ValidationError};
use conary_core::ccs::verify::{TrustViolation, VerificationSubject, VerifyError};

pub(super) fn from_error(error: &anyhow::Error) -> Option<Diagnostic> {
    let mut diagnostic = if let Some(error) = error.downcast_ref::<VerifyError>() {
        verification(error)
    } else if let Some(error) = error.downcast_ref::<V3ValidationError>() {
        authority(error)
    } else if let Some(error) = error.downcast_ref::<HostCapabilityPreflightError>() {
        return Some(preflight(error));
    } else {
        return None;
    };
    if let Some(subject) = error.downcast_ref::<VerificationSubject>() {
        diagnostic
            .facts
            .insert(0, ("Package", subject.path.display().to_string()));
    }
    Some(diagnostic)
}

fn verification(error: &VerifyError) -> Diagnostic {
    match error {
        VerifyError::NotSigned => Diagnostic::new("CCS package is not signed.")
            .note("Obtain a signed package from its publisher."),
        VerifyError::UnsupportedAlgorithm { algorithm } => {
            Diagnostic::new("CCS signature algorithm is not supported.")
                .fact("Algorithm", algorithm)
                .note("Obtain a package signed with the required Ed25519 algorithm.")
        }
        VerifyError::InvalidSignatureFormat(detail) => {
            Diagnostic::new("CCS verification data is malformed.").fact("Detail", detail)
        }
        VerifyError::SignatureInvalid(detail) => {
            Diagnostic::new("CCS signature verification failed.")
                .fact("Detail", detail)
                .note("Obtain an intact signed package from a trusted source.")
        }
        VerifyError::TrustViolation(cause) => trust(cause),
        VerifyError::PayloadInvalid(detail) => {
            Diagnostic::new("CCS payload verification failed.").fact("Detail", detail)
        }
        VerifyError::PackageError(detail) => {
            Diagnostic::new("CCS package structure is invalid.").fact("Detail", detail)
        }
    }
}

fn trust(cause: &TrustViolation) -> Diagnostic {
    match cause {
        TrustViolation::NoTrustedKeys => {
            Diagnostic::new("No trusted CCS signing keys are configured.")
                .note("Configure signing keys verified through a trusted source before retrying.")
        }
        TrustViolation::DuplicateTrustedKey { public_key } => {
            Diagnostic::new("CCS trust policy contains a duplicate signing key.")
                .fact("Public key", public_key)
                .note("Remove the duplicate policy entry and retry.")
        }
        TrustViolation::UntrustedSigner { key_id, public_key } => {
            let mut diagnostic = Diagnostic::new("CCS package signer is not trusted.");
            if let Some(key_id) = key_id {
                diagnostic = diagnostic.fact("Claimed key ID", key_id);
            }
            diagnostic.fact("Public key", public_key)
                .note("Verify the signing key through a trusted source and use a policy that authorizes this package.")
        }
        TrustViolation::MissingTimestamp => Diagnostic::new(
            "CCS signature timestamp is required by policy.",
        )
        .note("Obtain a signed package with a timestamp that satisfies the configured policy."),
        TrustViolation::InvalidTimestamp { timestamp } => {
            Diagnostic::new("CCS signature timestamp is malformed.")
                .fact("Timestamp", timestamp)
                .note("Obtain a package with valid RFC 3339 signature metadata from its publisher.")
        }
        TrustViolation::FutureTimestamp { timestamp } => Diagnostic::new(
            "CCS signature timestamp is in the future.",
        )
        .fact("Timestamp", timestamp)
        .note("Check the system clock and the publisher's signature timestamp before retrying."),
        TrustViolation::ExpiredSignature {
            timestamp,
            age_seconds,
            max_age_seconds,
        } => Diagnostic::new("CCS signature exceeds the policy age limit.")
            .fact("Timestamp", timestamp)
            .fact("Age (seconds)", age_seconds.to_string())
            .fact("Maximum age (seconds)", max_age_seconds.to_string())
            .note("Obtain a package with a signature that satisfies the configured age limit."),
    }
}

fn preflight(error: &HostCapabilityPreflightError) -> Diagnostic {
    match error {
        HostCapabilityPreflightError::MissingCapability { requirement, hook } => {
            Diagnostic::new("A required host capability is missing.")
                .fact("Hook", *hook)
                .fact("Requirement", requirement.to_string())
                .note("Install or configure the required interface and rerun 'conary system init'.")
        }
        HostCapabilityPreflightError::InterfaceDrift {
            requirement,
            executable,
        } => Diagnostic::new("A recorded host interface is no longer valid.")
            .fact("Requirement", requirement.to_string())
            .fact("Executable", executable.display().to_string())
            .note("Rerun 'conary system init' to refresh the host capability inventory."),
        HostCapabilityPreflightError::InvalidExecutionRoot { root } => {
            Diagnostic::new("CCS hook execution requires a materialized selected root.")
                .fact("Root", root.display().to_string())
                .note("Use an absolute materialized root other than '/'.")
        }
    }
}

fn authority(error: &V3ValidationError) -> Diagnostic {
    let mut diagnostic = Diagnostic::new("CCS package authority is invalid.");
    for violation in &error.diagnostics {
        let code = match violation.code {
            V3DiagnosticCode::MissingAuthority => "missing-authority",
            V3DiagnosticCode::UnsupportedFormatVersion => "unsupported-format-version",
            V3DiagnosticCode::TomlOnlyAuthority => "toml-only-authority",
            V3DiagnosticCode::KindContractViolation => "kind-contract-violation",
            V3DiagnosticCode::ComponentAuthorityMismatch => "component-authority-mismatch",
            V3DiagnosticCode::IdentityUnstable => "identity-unstable",
            V3DiagnosticCode::ConversionNotNative => "conversion-not-native",
            V3DiagnosticCode::StructuralBudgetExceeded => "structural-budget-exceeded",
        };
        diagnostic = diagnostic
            .fact("Violation", &violation.message)
            .fact("Code", code);
        if let Some(field) = &violation.field {
            diagnostic = diagnostic.fact("Field", field);
        }
        if let Some(path) = &violation.path {
            diagnostic = diagnostic.fact("Path", path);
        }
        if !violation.suggestion.is_empty() {
            diagnostic = diagnostic.note(&violation.suggestion);
        }
    }
    diagnostic
}

#[cfg(test)]
mod tests;
