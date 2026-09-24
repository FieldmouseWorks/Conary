// apps/conary/src/commands/ccs/selected_capabilities.rs

//! Selected capability view of one CCS package.
//!
//! Component selection changes which signed payload entries an install
//! materializes. Capability facts must follow that same selection so planning
//! and persistence never certify a provide the selected payload does not ship.

use anyhow::Result;
use conary_core::ccs::CcsPackage;
use conary_core::packages::traits::PackageFormat;
use conary_core::repository::dependency_model::{ProvidedCapability, RepositoryCapabilityKind};
use std::collections::HashSet;
use std::path::PathBuf;

use super::payload_paths::sanitize_package_relative_path;

/// Exact capability providers a CCS component selection installs.
///
/// Starts from the package's signed `resolution_capabilities()`. A `File`
/// provide is dropped only when the package's signed file entries ship that
/// exact path in a component outside `selected_component_names`. A declared
/// path the package does not ship is source-format authority, not payload
/// ownership, and is kept. Non-`File` capabilities are never touched.
///
/// Paths are compared through the lexical package-path parser, never against
/// selected-root-resolved deployment paths, so a usr-merged root cannot change
/// which provide a signed entry owns.
pub(crate) fn selected_ccs_resolution_capabilities(
    pkg: &CcsPackage,
    selected_component_names: &[String],
) -> Result<Vec<ProvidedCapability>> {
    let selected_components: HashSet<&str> = selected_component_names
        .iter()
        .map(String::as_str)
        .collect();
    let mut unselected_shipped_paths: HashSet<PathBuf> = HashSet::new();
    for entry in pkg.file_entries() {
        if !selected_components.contains(entry.component.as_str()) {
            unselected_shipped_paths.insert(sanitize_package_relative_path(&entry.path)?);
        }
    }

    let mut capabilities = pkg.resolution_capabilities()?;
    capabilities.retain(|capability| {
        if capability.kind != RepositoryCapabilityKind::File {
            return true;
        }
        let Ok(path) = sanitize_package_relative_path(&capability.name) else {
            // A `File` provide that is not a valid package-relative path cannot
            // name a signed shipping entry; it remains source-format authority.
            return true;
        };
        !unselected_shipped_paths.contains(&path)
    });

    Ok(capabilities)
}
