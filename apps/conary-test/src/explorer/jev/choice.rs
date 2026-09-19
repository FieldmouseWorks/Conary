// apps/conary-test/src/explorer/jev/choice.rs

//! Choice response validation belongs to the provider adapter, not package authority.
//! The official generated schema describes an approximately unit total:
//! https://github.com/typesafe-ai/typesafe-sdk-python/blob/2ce5c65f13646cab6e6f782328194c9d85f3300a/src/typesafe_sdk/_schemas/models.py#L11-L29
//! It specifies no numeric tolerance. One percentage point is our explicit
//! consumer limit, not a claimed upstream rounding guarantee. Never normalize
//! the map, replace the chosen ID, or infer execution permission from confidence.
use super::super::contract::DecisionRequest;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const POLICY: &str = "choice-approximate-total-v1";
pub const TOTAL_TOLERANCE: f64 = 0.01;
pub const FLOAT_SLACK: f64 = 1e-12;

#[derive(Deserialize)]
pub struct Answer {
    #[serde(rename = "type")]
    pub kind: String,
    pub choice: String,
    #[serde(deserialize_with = "unique_probabilities")]
    pub probabilities: BTreeMap<String, f64>,
    pub confidence: f64,
}

fn unique_probabilities<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, f64>, D::Error> {
    struct Unique;
    impl<'de> serde::de::Visitor<'de> for Unique {
        type Value = BTreeMap<String, f64>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a probability map without duplicate keys")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut map: M,
        ) -> Result<Self::Value, M::Error> {
            let mut values = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, f64>()? {
                if values.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom("duplicate choice probability key"));
                }
            }
            Ok(values)
        }
    }
    deserializer.deserialize_map(Unique)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    AcceptedExact,
    AcceptedApproximate,
    RejectedType,
    RejectedConfidence,
    RejectedCandidateKeys,
    RejectedProbabilityValue,
    RejectedTotal,
    RejectedChoiceNotMaximum,
}

#[derive(Debug, Serialize)]
pub struct Assessment {
    pub version: u32,
    pub policy: &'static str,
    pub outcome: Outcome,
    pub raw_total: Option<f64>,
    pub absolute_total_error: Option<f64>,
    pub total_tolerance: f64,
    pub floating_arithmetic_slack: f64,
}

impl Assessment {
    pub fn accepted(&self) -> bool {
        matches!(
            self.outcome,
            Outcome::AcceptedExact | Outcome::AcceptedApproximate
        )
    }
}

impl Answer {
    pub fn assess(&self, request: &DecisionRequest) -> Assessment {
        let total = self.probabilities.values().sum::<f64>();
        let deviation = (total - 1.0).abs();
        let outcome = if self.kind != "choice" {
            Outcome::RejectedType
        } else if !self.confidence.is_finite() || !(0.0..=1.0).contains(&self.confidence) {
            Outcome::RejectedConfidence
        } else if self.probabilities.len() != request.candidates.len()
            || !request
                .candidates
                .iter()
                .all(|c| self.probabilities.contains_key(&c.id))
        {
            Outcome::RejectedCandidateKeys
        } else if !self
            .probabilities
            .values()
            .all(|p| p.is_finite() && (0.0..=1.0).contains(p))
        {
            Outcome::RejectedProbabilityValue
        } else if !total.is_finite() || deviation > TOTAL_TOLERANCE + FLOAT_SLACK {
            Outcome::RejectedTotal
        } else if !self
            .probabilities
            .get(&self.choice)
            .is_some_and(|selected| self.probabilities.values().all(|p| p <= selected))
        {
            Outcome::RejectedChoiceNotMaximum
        } else if deviation <= FLOAT_SLACK {
            Outcome::AcceptedExact
        } else {
            Outcome::AcceptedApproximate
        };
        Assessment {
            version: 1,
            policy: POLICY,
            outcome,
            raw_total: total.is_finite().then_some(total),
            absolute_total_error: deviation.is_finite().then_some(deviation),
            total_tolerance: TOTAL_TOLERANCE,
            floating_arithmetic_slack: FLOAT_SLACK,
        }
    }
}
