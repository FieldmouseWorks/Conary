// apps/conary/src/ui/diagnostics/verification/tests.rs

use super::*;

#[test]
fn machine_signer_claim_remains_raw_while_human_facts_are_escaped() {
    let key_id = "release\nnote: claimed instruction\x1b";
    let error = anyhow::Error::new(VerifyError::TrustViolation(
        TrustViolation::UntrustedSigner {
            key_id: Some(key_id.into()),
            public_key: "key".into(),
        },
    ))
    .context(VerificationSubject {
        path: "/fixture/with\nnewline.ccs".into(),
    })
    .context("verify operation");
    let failure = verification_failure(&error);
    assert_eq!(
        failure.cause,
        Cause::UntrustedSigner {
            claimed_key_id: Some(key_id.into()),
            public_key: "key".into()
        }
    );
    assert_eq!(
        failure.archive_path.as_deref(),
        Some("/fixture/with\nnewline.ccs")
    );
    let human = super::super::from_error(&error);
    assert_eq!(failure.summary, human.message);
    assert_eq!(failure.notes, human.notes);
    assert!(!human.plain_body().contains('\x1b'));
    assert!(!serde_json::to_string(&failure).unwrap().contains('\x1b'));
}

#[test]
fn authority_projection_retains_every_field_without_human_round_trip() {
    let mut diagnostic = conary_core::ccs::v3::V3Diagnostic::error(
        V3DiagnosticCode::KindContractViolation,
        "raw\nmessage",
        Some("kind".into()),
        "suggestion",
    );
    diagnostic.path = Some("/fixture/payload".into());
    let error = anyhow::Error::new(V3ValidationError {
        diagnostics: vec![diagnostic.clone()],
    });
    let Cause::AuthorityInvalid { diagnostics } = verification_failure(&error).cause else {
        panic!("wrong cause")
    };
    assert_eq!(
        diagnostics,
        vec![CcsAuthorityDiagnostic {
            code: Code::KindContractViolation,
            severity: Severity::Error,
            message: diagnostic.message,
            field: diagnostic.field,
            path: diagnostic.path,
            invalid: diagnostic.invalid,
            suggestion: diagnostic.suggestion,
        }]
    );
}

#[test]
fn policy_limits_and_unclassified_chains_are_not_inferred_from_text() {
    let error = anyhow::Error::new(VerifyError::TrustViolation(
        TrustViolation::ExpiredSignature {
            timestamp: "timestamp".into(),
            age_seconds: 45,
            max_age_seconds: u64::MAX,
        },
    ))
    .context(TrustPolicySubject {
        path: "/fixture/policy.toml".into(),
    });
    let failure = verification_failure(&error);
    assert_eq!(failure.policy_path.as_deref(), Some("/fixture/policy.toml"));
    assert_eq!(
        failure.cause,
        Cause::ExpiredSignature {
            timestamp: "timestamp".into(),
            age_seconds: 45,
            max_age_seconds: u64::MAX
        }
    );
    let untyped = anyhow::anyhow!("CCS package signer is not trusted").context("caller");
    assert_eq!(
        verification_failure(&untyped).cause,
        Cause::Unclassified {
            causes: vec!["caller".into(), "CCS package signer is not trusted".into()]
        }
    );
}

#[test]
fn all_verification_variants_have_distinct_contract_causes() {
    let cases = [
        (VerifyError::NotSigned, Cause::NotSigned),
        (
            VerifyError::InvalidSignatureFormat("bad".into()),
            Cause::InvalidSignatureFormat {
                detail: "bad".into(),
            },
        ),
        (
            VerifyError::SignatureInvalid("bad".into()),
            Cause::SignatureInvalid {
                detail: "bad".into(),
            },
        ),
        (
            VerifyError::UnsupportedAlgorithm {
                algorithm: "algorithm".into(),
            },
            Cause::UnsupportedAlgorithm {
                algorithm: "algorithm".into(),
            },
        ),
        (
            VerifyError::PayloadInvalid("payload".into()),
            Cause::PayloadInvalid {
                detail: "payload".into(),
            },
        ),
        (
            VerifyError::PackageError("structure".into()),
            Cause::PackageInvalid {
                detail: "structure".into(),
            },
        ),
        (
            VerifyError::TrustViolation(TrustViolation::NoTrustedKeys),
            Cause::NoTrustedKeys,
        ),
        (
            VerifyError::TrustViolation(TrustViolation::DuplicateTrustedKey {
                public_key: "key".into(),
            }),
            Cause::DuplicateTrustedKey {
                public_key: "key".into(),
            },
        ),
        (
            VerifyError::TrustViolation(TrustViolation::MissingTimestamp),
            Cause::MissingTimestamp,
        ),
        (
            VerifyError::TrustViolation(TrustViolation::InvalidTimestamp {
                timestamp: "bad".into(),
            }),
            Cause::InvalidTimestamp {
                timestamp: "bad".into(),
            },
        ),
        (
            VerifyError::TrustViolation(TrustViolation::FutureTimestamp {
                timestamp: "future".into(),
            }),
            Cause::FutureTimestamp {
                timestamp: "future".into(),
            },
        ),
    ];
    for (error, expected) in cases {
        assert_eq!(verification_failure(&error.into()).cause, expected);
    }
}
