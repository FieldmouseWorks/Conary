//! Saved decisions from before a judge step carried its reasons name a bare
//! `Judge`. They load as a judge whose reasons weren't recorded.
use super::Step;
use serde::{Deserialize, Deserializer};

pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Step, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Saved {
        Legacy(Legacy),
        Step(Step),
    }
    #[derive(Deserialize)]
    enum Legacy {
        Judge,
    }
    Ok(match Saved::deserialize(deserializer)? {
        Saved::Legacy(Legacy::Judge) => Step::Judge(Vec::new()),
        Saved::Step(step) => step,
    })
}
