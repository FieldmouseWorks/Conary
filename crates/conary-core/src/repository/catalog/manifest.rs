// crates/conary-core/src/repository/catalog/manifest.rs

//! Classify retired catalog authority before decoding current manifest bodies.

use serde::Deserialize;

use super::{
    PROFILE_REVISION_SCHEMA_V4, ProfileRevisionV2, SOURCE_CATALOG_PROJECTION_VERSION_V3,
    SOURCE_SNAPSHOT_SCHEMA_V1, SourceSnapshotV1,
};
use crate::error::{Error, Result};

pub(super) fn require_current_source_projection(found: u32) -> Result<()> {
    match found {
        SOURCE_CATALOG_PROJECTION_VERSION_V3 => Ok(()),
        1..SOURCE_CATALOG_PROJECTION_VERSION_V3 => Err(Error::SourceProjectionRebuildRequired {
            found,
            current: SOURCE_CATALOG_PROJECTION_VERSION_V3,
        }),
        _ => Err(Error::ConfigError(format!(
            "unsupported source parser projection {found}"
        ))),
    }
}

pub(crate) fn require_current_profile_schema(found: u32) -> Result<()> {
    match found {
        PROFILE_REVISION_SCHEMA_V4 => Ok(()),
        1..PROFILE_REVISION_SCHEMA_V4 => Err(Error::ProfileRevisionRebuildRequired {
            found,
            current: PROFILE_REVISION_SCHEMA_V4,
        }),
        _ => Err(Error::ConfigError(format!(
            "unsupported profile revision schema {found}"
        ))),
    }
}

/// Strict readers receive typed rebuild requests for retired source projections.
/// Their nested bodies are never decoded through the current source contract.
pub fn decode_source_snapshot_manifest(bytes: &[u8]) -> Result<SourceSnapshotV1> {
    #[derive(Deserialize)]
    struct Header {
        schema_version: u32,
        parser_projection_version: u32,
    }
    let header: Header = serde_json::from_slice(bytes)
        .map_err(|error| Error::ParseError(format!("decode source manifest envelope: {error}")))?;
    if header.schema_version != SOURCE_SNAPSHOT_SCHEMA_V1 {
        return Err(Error::ConfigError(format!(
            "unsupported source snapshot schema {}",
            header.schema_version
        )));
    }
    require_current_source_projection(header.parser_projection_version)?;
    let manifest: SourceSnapshotV1 = serde_json::from_slice(bytes)
        .map_err(|error| Error::ParseError(format!("decode current source manifest: {error}")))?;
    manifest.validate()?;
    Ok(manifest)
}

/// Strict readers receive typed rebuild requests for retired profile schemas.
pub fn decode_profile_revision_manifest(bytes: &[u8]) -> Result<ProfileRevisionV2> {
    #[derive(Deserialize)]
    struct Header {
        schema_version: u32,
    }
    let header: Header = serde_json::from_slice(bytes)
        .map_err(|error| Error::ParseError(format!("decode profile manifest envelope: {error}")))?;
    require_current_profile_schema(header.schema_version)?;
    let manifest: ProfileRevisionV2 = serde_json::from_slice(bytes)
        .map_err(|error| Error::ParseError(format!("decode current profile manifest: {error}")))?;
    manifest.validate()?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retired_bodies_are_rebuild_requests_without_compatibility_decoding() {
        for found in 1..SOURCE_CATALOG_PROJECTION_VERSION_V3 {
            let bytes = format!(
                r#"{{"schema_version":1,"parser_projection_version":{found},"retired_body":null}}"#
            );
            assert!(matches!(decode_source_snapshot_manifest(bytes.as_bytes()),
                Err(Error::SourceProjectionRebuildRequired { found: actual, current: 3 }) if actual == found));
        }
        for found in 1..PROFILE_REVISION_SCHEMA_V4 {
            let bytes = format!(r#"{{"schema_version":{found},"retired_body":null}}"#);
            assert!(matches!(decode_profile_revision_manifest(bytes.as_bytes()),
                Err(Error::ProfileRevisionRebuildRequired { found: actual, current: 4 }) if actual == found));
        }
    }

    #[test]
    fn malformed_or_future_envelopes_do_not_acquire_rebuild_classification() {
        for version in ["0", "5", "-1", "1.5", "\"3\"", "null"] {
            let bytes = format!(r#"{{"schema_version":{version}}}"#);
            assert!(!matches!(
                decode_profile_revision_manifest(bytes.as_bytes()),
                Err(Error::ProfileRevisionRebuildRequired { .. }) | Ok(_)
            ));
        }
        let duplicate =
            br#"{"schema_version":1,"parser_projection_version":2,"parser_projection_version":3}"#;
        assert!(!matches!(
            decode_source_snapshot_manifest(duplicate),
            Err(Error::SourceProjectionRebuildRequired { .. }) | Ok(_)
        ));
    }
}
