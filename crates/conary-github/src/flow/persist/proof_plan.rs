//! Saved decisions from before proofs were prepared outside the proof slots
//! name plans the flow no longer makes: `Evaluate`, `Build`, `Run`, `Job`,
//! `Prebuild` and `Stored`. A saved decision is a report of what that pass
//! decided, never read back to decide, so such a plan loads as no plan, and
//! the rest of its record (jobs, permissions, memory) loads unchanged.
use super::super::{ProofPlan, Recipe};
use serde::{Deserialize, Deserializer};
use std::collections::{BTreeMap, BTreeSet};

pub fn deserialize<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<ProofPlan>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    #[allow(dead_code)]
    enum Saved {
        Plan(ProofPlan),
        Legacy(Legacy),
    }
    // Their old shapes, so a record that was never valid still fails to load.
    #[derive(Deserialize)]
    #[allow(dead_code)]
    struct Derivation {
        path: String,
        outputs: BTreeSet<String>,
    }
    #[derive(Deserialize)]
    #[allow(dead_code)]
    enum Legacy {
        Evaluate(BTreeMap<String, Recipe>),
        Build(BTreeMap<String, Derivation>),
        Run(BTreeMap<String, Recipe>),
        Job(BTreeMap<String, Recipe>),
        Prebuild(BTreeSet<String>),
        Stored(BTreeMap<String, Derivation>),
    }
    Ok(match Option::<Saved>::deserialize(deserializer)? {
        Some(Saved::Plan(plan)) => Some(plan),
        Some(Saved::Legacy(_)) | None => None,
    })
}
