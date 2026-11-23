//! Saved decisions from before a judge step carried its reasons name a bare
//! `Judge`. They load as a judge whose reasons weren't recorded. Saved
//! decisions from when Renovate merged its own PRs name `Settle`, the merge
//! left to it. They load as the flow's own merge, which is what such a PR
//! gets now. Saved decisions from before a listing named its door name a
//! bare `ListOwner`: they load as the wait they don't say the door of, and
//! the next pass decides again.
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
        ListOwner,
    }
    Ok(match Saved::deserialize(deserializer)? {
        Saved::Legacy(Legacy::Judge) => Step::Judge(Vec::new()),
        Saved::Legacy(Legacy::Settle) => Step::Merge,
        Saved::Legacy(Legacy::ListOwner) => Step::Wait { until: None },
        Saved::Step(step) => step,
    })
}
