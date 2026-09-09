// apps/conary/src/commands/package_target/release.rs

use conary_core::repository::versioning::validate_package_release;
use std::str::FromStr;

/// Exact installed package release selector.
///
/// `Unspecified` matches only installed troves that carry no CCS package
/// release. `Exact` matches the identical release string; validated releases
/// are retained verbatim, so leading zeros are never normalized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstalledRelease {
    Exact(String),
    Unspecified,
}

impl FromStr for InstalledRelease {
    type Err = String;

    fn from_str(raw: &str) -> std::result::Result<Self, Self::Err> {
        if raw == "none" {
            return Ok(Self::Unspecified);
        }
        validate_package_release(raw)
            .map_err(|error| format!("invalid package release '{raw}': {error}"))?;
        Ok(Self::Exact(raw.to_string()))
    }
}
