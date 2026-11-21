//! Saved decisions from before a judge step carried its reasons name a bare
//! `Judge`. They load as a judge whose reasons weren't recorded. Saved
//! decisions from when Renovate merged its own PRs name `Settle`, the merge
//! left to it. They load as the flow's own merge, which is what such a PR
//! gets now.
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
        Settle,
    }
    Ok(match Saved::deserialize(deserializer)? {
        Saved::Legacy(Legacy::Judge) => Step::Judge(Vec::new()),
        Saved::Legacy(Legacy::Settle) => Step::Merge,
        Saved::Step(step) => step,
    })
}
