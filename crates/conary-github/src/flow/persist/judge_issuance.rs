//! Compatibility belongs to saved flow state, never the plugin wire type.
use super::{JudgeIssuance, JudgeResult, Judgment, Revision};
use conary_core::plugin::Provenance;
use serde::{Deserialize, Deserializer, de::Error};

// Always write the contract's canonical words through Judgment's serializer.
pub use super::pairs::serialize;

/// A saved verdict: a bare word (older records, or no provenance), or a
/// canonical word with the provenance of the judge that issued it. Only bare
/// words predate the contract's vocabulary.
#[derive(Deserialize)]
#[serde(untagged)]
enum Saved {
    Recorded(Recorded),
    Word(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Recorded {
    verdict: Judgment,
    provenance: Provenance,
}

pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<JudgeIssuance, D::Error> {
    let entries = Vec::<(Revision, (u64, Option<Saved>))>::deserialize(deserializer)?;
    entries
        .into_iter()
        .map(|(revision, (generation, saved))| {
            let result = saved
                .map(|saved| match saved {
                    Saved::Word(word) => word_of::<D::Error>(&word).map(JudgeResult::Legacy),
                    Saved::Recorded(Recorded {
                        verdict,
                        provenance,
                    }) => Ok(JudgeResult::Recorded {
                        verdict,
                        provenance,
                    }),
                })
                .transpose()?;
            Ok((revision, (generation, result)))
        })
        .collect()
}

fn word_of<E: Error>(word: &str) -> Result<Judgment, E> {
    match word {
        "Land" => Some(Judgment::Pass),
        "Failing" => Some(Judgment::Fail),
        "Owner" => Some(Judgment::Owner),
        "Misdescribed" => Some(Judgment::Misdescribed),
        _ => Judgment::from_word(word),
    }
    .ok_or_else(|| E::custom(format!("unknown persisted judgment {word:?}")))
}
