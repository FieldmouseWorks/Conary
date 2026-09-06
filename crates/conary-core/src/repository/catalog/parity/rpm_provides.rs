// crates/conary-core/src/repository/catalog/parity/rpm_provides.rs

//! libsolv's ordered set of declared RPM provider IDs.

use std::collections::BTreeSet;

use crate::error::{Error, Result};
use crate::repository::catalog::CatalogProvideRecordV1;
use crate::repository::dependency_model::{CapabilityProvenance, SourcePackageFormat};
use crate::repository::versioning::VersionScheme;

pub(super) fn native_provides(
    scheme: VersionScheme,
    provides: Vec<CatalogProvideRecordV1>,
) -> Result<Vec<CatalogProvideRecordV1>> {
    if scheme != VersionScheme::Rpm {
        return Ok(provides);
    }
    let mut declared = Vec::new();
    let mut projected = Vec::new();
    for provide in provides {
        if let CapabilityProvenance::SourceDeclared {
            format: SourcePackageFormat::Rpm,
            record_index,
        } = provide.provenance
        {
            declared.push((record_index, provide));
        } else {
            projected.push(provide);
        }
    }
    declared.sort_by_key(|(index, _)| *index);
    let mut seen = BTreeSet::new();
    for (source_index, (record_index, mut provide)) in declared.into_iter().enumerate() {
        if usize::try_from(record_index).ok() != Some(source_index) {
            return Err(Error::ConflictError(
                "RPM declared provider source indices are not contiguous and unique".into(),
            ));
        }
        // ext/repo_rpmmd.c:adddep uses src/repo.c:repo_addid_dep with
        // marker zero: repeated IDs retain their first occurrence. Native
        // producer provenance indexes this deduplicated dependency array.
        // Compare every other fact before removing a source declaration.
        provide.provenance = CapabilityProvenance::SourceDeclared {
            format: SourcePackageFormat::Rpm,
            record_index: 0,
        };
        let signature = key(&provide)?;
        let native_index = u32::try_from(seen.len())
            .map_err(|_| Error::ParseError("native RPM provide index exceeds u32".into()))?;
        if !seen.insert(signature) {
            continue;
        }
        provide.provenance = CapabilityProvenance::SourceDeclared {
            format: SourcePackageFormat::Rpm,
            record_index: native_index,
        };
        projected.push(provide);
    }
    let mut keyed = projected
        .into_iter()
        .map(|provide| Ok((key(&provide)?, provide)))
        .collect::<Result<Vec<_>>>()?;
    keyed.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(keyed.into_iter().map(|(_, provide)| provide).collect())
}

fn key(provide: &CatalogProvideRecordV1) -> Result<Vec<u8>> {
    crate::json::canonical_json(provide)
        .map_err(|error| Error::ParseError(format!("encode RPM provider projection: {error}")))
}
