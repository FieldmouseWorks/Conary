// apps/conary/src/ui/diagnostics/ccs/tests.rs

use super::*;
use conary_core::ccs::HostCapabilityRequirement;
use conary_core::ccs::v3::V3Diagnostic;
use std::path::PathBuf;

#[test]
fn typed_signer_and_path_replace_context_prose_without_claiming_trust() {
    for key_id in [None, Some("unverified-label".to_string())] {
        let error = anyhow::Error::new(VerifyError::TrustViolation(
            TrustViolation::UntrustedSigner {
                key_id: key_id.clone(),
                public_key: "exact-key".into(),
            },
        ))
        .context(VerificationSubject {
            path: PathBuf::from("/fixture/package.ccs"),
        })
        .context("outer install context");
        let diagnostic = from_error(&error).unwrap();
        assert_eq!(diagnostic.message, "CCS package signer is not trusted.");
        assert_eq!(
            diagnostic.facts[0],
            ("Package", "/fixture/package.ccs".to_owned())
        );
        assert_eq!(
            diagnostic.facts.last().unwrap(),
            &("Public key", "exact-key".to_owned())
        );
        assert_eq!(
            diagnostic
                .facts
                .iter()
                .find(|(label, _)| *label == "Claimed key ID")
                .map(|(_, value)| value),
            key_id.as_ref()
        );
        assert_eq!(diagnostic.notes.len(), 1);
        assert!(!diagnostic.plain_body().contains("outer install"));
    }
    assert!(
        from_error(&anyhow::anyhow!(
            "CCS v3 package signer is not trusted: key_id=Some(\"release\")"
        ))
        .is_none()
    );
}

#[test]
fn trust_causes_keep_timestamp_limits_and_safe_actions() {
    let expired = trust(&TrustViolation::ExpiredSignature {
        timestamp: "2000-01-01T00:00:00Z".into(),
        age_seconds: 100,
        max_age_seconds: 60,
    });
    assert_eq!(
        expired.facts,
        [
            ("Timestamp", "2000-01-01T00:00:00Z".into()),
            ("Age (seconds)", "100".into()),
            ("Maximum age (seconds)", "60".into())
        ]
    );
    for cause in [
        TrustViolation::NoTrustedKeys,
        TrustViolation::DuplicateTrustedKey {
            public_key: "key".into(),
        },
        TrustViolation::MissingTimestamp,
        TrustViolation::InvalidTimestamp {
            timestamp: "invalid".into(),
        },
        TrustViolation::FutureTimestamp {
            timestamp: "future".into(),
        },
    ] {
        let diagnostic = trust(&cause);
        assert_eq!(diagnostic.notes.len(), 1);
        assert!(!diagnostic.plain_body().contains("--force"));
    }
}

#[test]
fn authority_renderer_retains_all_codes_fields_paths_and_suggestions() {
    let mut first = V3Diagnostic::error(
        V3DiagnosticCode::KindContractViolation,
        "first violation",
        Some("kind".into()),
        "first action",
    );
    first.path = Some("/fixture/payload".into());
    let second = V3Diagnostic::error(
        V3DiagnosticCode::IdentityUnstable,
        "second violation",
        Some("name".into()),
        "second action",
    );
    let diagnostic = authority(&V3ValidationError {
        diagnostics: vec![first, second],
    });
    assert_eq!(
        diagnostic.facts,
        [
            ("Violation", "first violation".into()),
            ("Code", "kind-contract-violation".into()),
            ("Field", "kind".into()),
            ("Path", "/fixture/payload".into()),
            ("Violation", "second violation".into()),
            ("Code", "identity-unstable".into()),
            ("Field", "name".into()),
        ]
    );
    assert_eq!(diagnostic.notes, ["first action", "second action"]);
}

#[test]
fn capability_preflight_preserves_requirement_and_affected_path() {
    let missing = HostCapabilityPreflightError::MissingCapability {
        requirement: HostCapabilityRequirement::Ldconfig,
        hook: "ldconfig",
    };
    let error = anyhow::Error::new(missing).context("execute package hook");
    let diagnostic = from_error(&error).unwrap();
    assert_eq!(
        diagnostic.facts,
        [
            ("Hook", "ldconfig".into()),
            (
                "Requirement",
                HostCapabilityRequirement::Ldconfig.to_string()
            )
        ]
    );
    let drift = preflight(&HostCapabilityPreflightError::InterfaceDrift {
        requirement: HostCapabilityRequirement::Ldconfig,
        executable: "/fixture/ldconfig".into(),
    });
    assert_eq!(drift.facts[1], ("Executable", "/fixture/ldconfig".into()));
    let root = preflight(&HostCapabilityPreflightError::InvalidExecutionRoot { root: "/".into() });
    assert_eq!(root.facts, [("Root", "/".into())]);
}
