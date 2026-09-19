// apps/conary-test/src/explorer/tests/jev_choice.rs
#![cfg(test)]

use super::*;
use crate::explorer::jev::choice::{Answer, Outcome};

fn answer(request: &DecisionRequest) -> Answer {
    Answer {
        kind: "choice".into(),
        choice: "c0".into(),
        confidence: 1.0,
        probabilities: request
            .candidates
            .iter()
            .map(|c| (c.id.clone(), if c.id == "c0" { 1.0 } else { 0.0 }))
            .collect(),
    }
}

#[test]
fn ambiguous_duplicate_probability_keys_are_malformed() {
    let raw = r#"{"type":"choice","choice":"c0","confidence":1,"probabilities":{"c0":0,"c0":1}}"#;
    assert!(serde_json::from_str::<Answer>(raw).is_err());
}

#[test]
fn approximate_total_has_a_fixed_inclusive_bound_and_never_changes_the_map() {
    let request = DecisionRequest::new(observation(), 8).unwrap();
    for (total, expected) in [
        (0.0, Outcome::RejectedTotal),
        (0.98, Outcome::RejectedTotal),
        (0.989999, Outcome::RejectedTotal),
        (0.99, Outcome::AcceptedApproximate),
        (0.999, Outcome::AcceptedApproximate),
        (1.0, Outcome::AcceptedExact),
        (1.001, Outcome::AcceptedApproximate),
        (1.01, Outcome::AcceptedApproximate),
        (1.010001, Outcome::RejectedTotal),
        (1.02, Outcome::RejectedTotal),
        (2.0, Outcome::RejectedTotal),
    ] {
        let mut answer = answer(&request);
        answer.probabilities.insert("c0".into(), total / 2.0);
        answer.probabilities.insert("c1".into(), total / 2.0);
        let original = answer.probabilities.clone();
        let result = answer.assess(&request);
        assert_eq!(result.outcome, expected, "total {total}");
        assert_eq!(answer.probabilities, original);
        assert_eq!(answer.choice, "c0"); // Tied maxima are permitted, never reselected.
        assert_eq!(result.raw_total, Some(total));
    }
}

#[test]
fn value_key_confidence_type_and_maximum_checks_cannot_be_bypassed_by_a_good_total() {
    let request = DecisionRequest::new(observation(), 8).unwrap();
    for invalid in [-0.01, 1.01, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut value = answer(&request);
        value.probabilities.insert("c0".into(), invalid);
        value.probabilities.insert("c1".into(), 1.0 - invalid);
        let result = value.assess(&request);
        assert_eq!(result.outcome, Outcome::RejectedProbabilityValue);
        assert!(serde_json::to_string(&result).is_ok());
        value = answer(&request);
        value.confidence = invalid;
        assert_eq!(value.assess(&request).outcome, Outcome::RejectedConfidence);
    }
    for change in ["missing", "extra", "rebound"] {
        let mut value = answer(&request);
        if change != "extra" {
            value.probabilities.remove("c1");
        }
        if change != "missing" {
            value.probabilities.insert("forged".into(), 0.0);
        }
        assert_eq!(
            value.assess(&request).outcome,
            Outcome::RejectedCandidateKeys
        );
    }
    let mut value = answer(&request);
    value.kind = "score".into();
    assert_eq!(value.assess(&request).outcome, Outcome::RejectedType);
    for choice in ["c1", "unknown", "c0; shell-text"] {
        value = answer(&request);
        value.choice = choice.into();
        assert_eq!(
            value.assess(&request).outcome,
            Outcome::RejectedChoiceNotMaximum
        );
    }
}

#[test]
fn preserved_live_probability_maps_match_the_explicit_approximate_contract() {
    // Numeric observations from the closed pilot; no credential or live HTTP.
    for (fixtures, probabilities, selected) in [
        (
            vec![Fixture::AppV2, Fixture::Companion],
            vec![0.0, 0.0, 0.16, 0.11, 0.15, 0.5700000000000001, 0.0],
            "c5",
        ),
        (
            vec![Fixture::AppV1],
            vec![0.0, 0.0, 0.04, 0.22, 0.68, 0.05, 0.0, 0.0],
            "c4",
        ),
    ] {
        let mut observed = observation();
        for fixture in fixtures {
            observed
                .facts
                .packages
                .insert(fixture.package(), fixture.version().into());
        }
        let request = DecisionRequest::new(observed, 8).unwrap();
        assert_eq!(request.candidates.len(), probabilities.len());
        let answer = Answer {
            kind: "choice".into(),
            choice: selected.into(),
            confidence: 0.49,
            probabilities: probabilities
                .into_iter()
                .enumerate()
                .map(|(i, p)| (format!("c{i}"), p))
                .collect(),
        };
        let result = answer.assess(&request);
        assert_eq!(result.outcome, Outcome::AcceptedApproximate);
        assert!((result.raw_total.unwrap() - 0.99).abs() < 1e-12);
        assert!(
            request
                .authorize(
                    &Decision {
                        candidate_id: answer.choice,
                        binding: request.binding.clone()
                    },
                    &request.observation
                )
                .is_ok()
        );
    }
}
