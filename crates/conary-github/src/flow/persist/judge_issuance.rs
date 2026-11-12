//! Compatibility belongs to saved flow state, never the plugin wire type.
use super::{Judgment, Revision};
use serde::{Deserialize, Deserializer, de::Error};
use std::collections::BTreeMap;

// Always write the contract's canonical words through Judgment's serializer.
pub use super::pairs::serialize;

pub fn deserialize<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<Revision, (u64, Option<Judgment>)>, D::Error> {
    let entries = Vec::<(Revision, (u64, Option<String>))>::deserialize(deserializer)?;
    entries
        .into_iter()
        .map(|(revision, (generation, verdict))| {
            let verdict = verdict
                .map(|word| {
                    match word.as_str() {
                        "Land" => Some(Judgment::Pass),
                        "Failing" => Some(Judgment::Fail),
                        "Owner" => Some(Judgment::Owner),
                        "Misdescribed" => Some(Judgment::Misdescribed),
                        _ => Judgment::from_word(&word),
                    }
                    .ok_or_else(|| D::Error::custom(format!("unknown persisted judgment {word:?}")))
                })
                .transpose()?;
            Ok((revision, (generation, verdict)))
        })
        .collect()
}
