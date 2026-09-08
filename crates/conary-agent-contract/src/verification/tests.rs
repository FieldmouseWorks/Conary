// crates/conary-agent-contract/src/verification/tests.rs

use super::*;
use serde_json::json;

fn failure() -> CcsVerificationReport {
    CcsVerificationReport {
        schema: CcsVerificationSchema::V1,
        package: "/fixture/archive.ccs".into(),
        policy: Some("/fixture/policy.toml".into()),
        outcome: CcsVerificationOutcome::Failed {
            failure: CcsVerificationFailure {
                cause: CcsVerificationCause::UntrustedSigner {
                    claimed_key_id: Some("claim\nwith controls\u{1b}".into()),
                    public_key: "exact-key".into(),
                },
                summary: "Signer is not trusted".into(),
                notes: vec![],
                archive_path: Some("/fixture/archive.ccs".into()),
                policy_path: None,
            },
        },
    }
}

#[test]
fn failure_round_trip_preserves_raw_claims_and_exact_limits() {
    let report = failure();
    let encoded = serde_json::to_string(&report).unwrap();
    assert!(!encoded.contains('\x1b'));
    assert_eq!(
        serde_json::from_str::<CcsVerificationReport>(&encoded).unwrap(),
        report
    );
    let cause = CcsVerificationCause::ExpiredSignature {
        timestamp: "timestamp".into(),
        age_seconds: 12,
        max_age_seconds: u64::MAX,
    };
    assert_eq!(
        serde_json::from_value::<CcsVerificationCause>(serde_json::to_value(&cause).unwrap())
            .unwrap(),
        cause
    );
}

#[test]
fn result_rejects_unknown_version_fields_and_mixed_outcomes() {
    let value = serde_json::to_value(failure()).unwrap();
    let mut future = value.clone();
    future["schema"] = json!("conary.ccs.verification.v2");
    assert!(serde_json::from_value::<CcsVerificationReport>(future).is_err());
    let mut unknown = value.clone();
    unknown["trusted"] = json!(true);
    assert!(serde_json::from_value::<CcsVerificationReport>(unknown).is_err());
    let mut mixed = value.clone();
    mixed["outcome"]["facts"] = json!({});
    assert!(serde_json::from_value::<CcsVerificationReport>(mixed).is_err());
    let mut unknown_cause = value.clone();
    unknown_cause["outcome"]["failure"]["cause"]["kind"] = json!("guessed_cause");
    assert!(serde_json::from_value::<CcsVerificationReport>(unknown_cause).is_err());
    let mut nested = value;
    nested["outcome"]["failure"]["cause"]["override_trust"] = json!(true);
    assert!(serde_json::from_value::<CcsVerificationReport>(nested).is_err());
    assert!(
        serde_json::from_value::<CcsVerificationRequest>(json!({"package":"fixture"})).is_err()
    );
    assert!(
        serde_json::from_value::<CcsVerificationRequest>(
            json!({"package":"fixture","policy":"policy","allow_unsigned":true})
        )
        .is_err()
    );
}

#[test]
fn schema_declares_version_and_disjoint_outcomes() {
    let schema = serde_json::to_value(schemars::schema_for!(CcsVerificationReport)).unwrap();
    assert_eq!(schema["additionalProperties"], false);
    assert!(schema.to_string().contains("conary.ccs.verification.v1"));
    let outcome = &schema["$defs"]["CcsVerificationOutcome"];
    assert_eq!(outcome["oneOf"].as_array().unwrap().len(), 2);
}
