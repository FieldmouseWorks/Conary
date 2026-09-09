// apps/conary/src/commands/install/prepare.rs
//! Package parsing and pre-installation validation

use super::{InstallIntent, InstallSemantics};
use crate::commands::PackageFormatType;
use anyhow::{Context, Result};
use conary_core::components::ComponentType;
use conary_core::db::models::Trove;
use conary_core::packages::PackageFormat;
use conary_core::packages::arch::ArchPackage;
use conary_core::packages::deb::DebPackage;
use conary_core::packages::rpm::RpmPackage;
use conary_core::repository::selector::package_architectures_match;
use conary_core::repository::versioning::{VersionScheme, compare_package_identities};
use rusqlite::Connection;
use std::cmp::Ordering;
use std::path::Path;
use tracing::{info, warn};

/// Parse a package file and return the appropriate parser
pub fn parse_package(path: &Path, format: PackageFormatType) -> Result<Box<dyn PackageFormat>> {
    let path_str = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Invalid package path (non-UTF8)"))?;

    let pkg: Box<dyn PackageFormat> = match format {
        PackageFormatType::Rpm => Box::new(
            RpmPackage::parse(path_str)
                .with_context(|| format!("Failed to parse RPM package '{}'", path_str))?,
        ),
        PackageFormatType::Deb => Box::new(
            DebPackage::parse(path_str)
                .with_context(|| format!("Failed to parse DEB package '{}'", path_str))?,
        ),
        PackageFormatType::Arch => Box::new(
            ArchPackage::parse(path_str)
                .with_context(|| format!("Failed to parse Arch package '{}'", path_str))?,
        ),
        PackageFormatType::Eopkg => Box::new(
            conary_core::packages::eopkg::EopkgPackage::parse(path_str)
                .with_context(|| format!("Failed to parse eopkg package '{}'", path_str))?,
        ),
    };

    info!(
        "Parsed package: {} version {} ({} files, {} dependencies)",
        pkg.name(),
        pkg.version(),
        pkg.files().len(),
        pkg.requirements().len()
    );

    Ok(pkg)
}

/// Result of checking for existing package installation
pub enum UpgradeCheck {
    /// Fresh install - no existing package
    FreshInstall,
    /// The exact package identity is already installed.
    AlreadyInstalled(Box<conary_core::db::models::Trove>),
    /// Upgrade from an older version (boxed to reduce enum size)
    Upgrade(Box<conary_core::db::models::Trove>),
    /// Downgrade to an older version (when --allow-downgrade is used)
    Downgrade(Box<conary_core::db::models::Trove>),
    /// Explicit replacement across package-manager version schemes.
    Replatform(Box<conary_core::db::models::Trove>),
}

/// Check if package is already installed and determine upgrade status
///
/// `replacement` is the exact installed record selected by an update. When it
/// is supplied, this function never falls back to the first name/architecture
/// match: the snapshot's persisted row is reloaded and revalidated under the
/// caller's existing mutation-lock/preparation boundary, and only that row may
/// be replaced. Ordinary installs pass `None` and keep first-match behavior.
pub fn check_upgrade_status(
    conn: &Connection,
    pkg: &dyn PackageFormat,
    semantics: &InstallSemantics,
    allow_downgrade: bool,
    intent: InstallIntent,
    replacement: Option<&Trove>,
) -> Result<UpgradeCheck> {
    if let Some(expected) = replacement {
        return check_explicit_replacement_status(
            conn,
            pkg,
            semantics,
            allow_downgrade,
            intent,
            expected,
        );
    }

    let existing = conary_core::db::models::Trove::find_by_name(conn, pkg.name())?;

    for trove in &existing {
        if architectures_share_install_slot(
            trove.version_scheme,
            trove.architecture.as_deref(),
            pkg.version_scheme(),
            pkg.architecture(),
        )? {
            return classify_installed_trove(trove, pkg, semantics, allow_downgrade, intent);
        }
    }

    Ok(UpgradeCheck::FreshInstall)
}

/// Exact update replacement: reload the selected row by ID and refuse when it
/// disappeared or no longer matches the snapshot the update planned against.
fn check_explicit_replacement_status(
    conn: &Connection,
    pkg: &dyn PackageFormat,
    semantics: &InstallSemantics,
    allow_downgrade: bool,
    intent: InstallIntent,
    expected: &Trove,
) -> Result<UpgradeCheck> {
    let current = revalidate_replacement_snapshot(conn, expected)?;
    let expected_id = current
        .id
        .context("revalidated replacement has no identity")?;
    if pkg.name() != current.name {
        anyhow::bail!(
            "Incoming package '{}' does not match update replacement target '{}'",
            pkg.name(),
            current.name
        );
    }
    if !architectures_share_install_slot(
        current.version_scheme,
        current.architecture.as_deref(),
        pkg.version_scheme(),
        pkg.architecture(),
    )? {
        anyhow::bail!(
            "Incoming package '{}' architecture '{}' is incompatible with update replacement target '{}' architecture '{}'",
            pkg.name(),
            pkg.architecture().unwrap_or("no-arch"),
            current.name,
            current.architecture.as_deref().unwrap_or("no-arch")
        );
    }

    check_replacement_identity_available(
        conn,
        expected_id,
        pkg.name(),
        pkg.version(),
        pkg.package_release(),
        pkg.version_scheme(),
        pkg.architecture(),
    )?;

    classify_installed_trove(&current, pkg, semantics, allow_downgrade, intent)
}

/// Reload prepared replacement authority at the caller's mutation boundary.
pub(super) fn revalidate_replacement_snapshot(
    conn: &Connection,
    expected: &Trove,
) -> Result<Trove> {
    let expected_id = expected.id.ok_or_else(|| {
        anyhow::anyhow!(
            "Update replacement target '{}' has no persisted installed record identity",
            expected.name
        )
    })?;
    let current = Trove::find_by_id(conn, expected_id)?.ok_or_else(|| {
        anyhow::anyhow!(
            "Update replacement target '{}-{}' (installed trove {expected_id}) disappeared before replacement",
            expected.name,
            expected.version
        )
    })?;
    if current.name != expected.name
        || current.version != expected.version
        || current.package_release != expected.package_release
        || current.architecture != expected.architecture
        || current.version_scheme != expected.version_scheme
        || current.source_profile != expected.source_profile
        || current.install_source != expected.install_source
        || current.trove_type != expected.trove_type
        || current.pinned != expected.pinned
        || current.installed_from_repository_id != expected.installed_from_repository_id
        || current.native_package_identity != expected.native_package_identity
        || current.debian_multi_arch != expected.debian_multi_arch
    {
        anyhow::bail!(
            "Update replacement target '{}' changed after selection; refusing to replace a different installed record",
            expected.name
        );
    }
    Ok(current)
}

pub(super) fn check_replacement_identity_available(
    conn: &Connection,
    expected_id: i64,
    name: &str,
    version: &str,
    release: Option<&str>,
    scheme: conary_core::repository::versioning::VersionScheme,
    architecture: Option<&str>,
) -> Result<()> {
    // The incoming exact identity must not already live on a different
    // installed row, or this replacement would duplicate it.
    for other in Trove::find_by_name(conn, name)? {
        if other.id == Some(expected_id) {
            continue;
        }
        if architectures_share_install_slot(
            other.version_scheme,
            other.architecture.as_deref(),
            scheme,
            architecture,
        )? && other.version == version
            && other.package_release.as_deref() == release
        {
            anyhow::bail!(
                "Package {} version {} ({}) is already installed as a separate record (installed trove {})",
                name,
                version,
                architecture.unwrap_or("no-arch"),
                other.id.unwrap_or_default()
            );
        }
    }

    Ok(())
}

fn classify_installed_trove(
    trove: &Trove,
    pkg: &dyn PackageFormat,
    semantics: &InstallSemantics,
    allow_downgrade: bool,
    intent: InstallIntent,
) -> Result<UpgradeCheck> {
    if trove.version == pkg.version() && trove.package_release.as_deref() == pkg.package_release() {
        return Ok(UpgradeCheck::AlreadyInstalled(Box::new(trove.clone())));
    }

    if trove.version_scheme != semantics.version_scheme {
        if intent == InstallIntent::Replatform {
            info!(
                "Replatforming {} from {} version scheme {} to {} version scheme {}",
                pkg.name(),
                trove.version_scheme.as_str(),
                trove.version,
                semantics.version_scheme.as_str(),
                pkg.version()
            );
            return Ok(UpgradeCheck::Replatform(Box::new(trove.clone())));
        }
        return Err(anyhow::anyhow!(
            "Cannot replace package {} across {} and {} version schemes without an explicit replatform operation",
            pkg.name(),
            trove.version_scheme.as_str(),
            semantics.version_scheme.as_str()
        ));
    }

    match compare_installed_and_incoming_versions(
        trove,
        pkg.version(),
        pkg.package_release(),
        semantics,
    )? {
        Ordering::Less => {
            info!(
                "Upgrading {} from version {} to {}",
                pkg.name(),
                trove.version,
                pkg.version()
            );
            Ok(UpgradeCheck::Upgrade(Box::new(trove.clone())))
        }
        Ordering::Equal | Ordering::Greater => {
            if allow_downgrade {
                warn!(
                    "Downgrading {} from version {} to {}",
                    pkg.name(),
                    trove.version,
                    pkg.version()
                );
                Ok(UpgradeCheck::Downgrade(Box::new(trove.clone())))
            } else {
                Err(anyhow::anyhow!(
                    "Cannot downgrade package {} from version {} to {} (use --allow-downgrade to override)",
                    pkg.name(),
                    trove.version,
                    pkg.version()
                ))
            }
        }
    }
}

fn architectures_share_install_slot(
    installed_scheme: VersionScheme,
    installed: Option<&str>,
    incoming_scheme: VersionScheme,
    incoming: Option<&str>,
) -> Result<bool> {
    Ok(match (installed, incoming) {
        (Some(installed), Some(incoming)) => package_architectures_match(
            installed_scheme,
            installed,
            incoming_scheme,
            incoming,
            &conary_core::repository::registry::detect_system_arch()?,
        ),
        _ => false,
    })
}

fn compare_installed_and_incoming_versions(
    trove: &Trove,
    incoming_version: &str,
    incoming_release: Option<&str>,
    semantics: &InstallSemantics,
) -> Result<Ordering> {
    Ok(compare_package_identities(
        trove.version_scheme,
        &trove.version,
        trove.package_release.as_deref(),
        semantics.version_scheme,
        incoming_version,
        incoming_release,
    )?)
}

pub(super) fn version_scheme_for_format(format: PackageFormatType) -> VersionScheme {
    match format {
        PackageFormatType::Rpm => VersionScheme::Rpm,
        PackageFormatType::Deb => VersionScheme::Debian,
        PackageFormatType::Arch => VersionScheme::Arch,
        PackageFormatType::Eopkg => VersionScheme::Eopkg,
    }
}

/// Represents which components to install
#[derive(Debug, Clone)]
pub enum ComponentSelection {
    /// Install only default components (runtime, lib, config)
    Defaults,
    /// Install all components
    All,
    /// Install specific component(s)
    Specific(Vec<ComponentType>),
}

impl ComponentSelection {
    /// Get a display string for the selection
    pub fn display(&self) -> String {
        match self {
            Self::All => "all".to_string(),
            Self::Defaults => "defaults (runtime, lib, config)".to_string(),
            Self::Specific(types) => types
                .iter()
                .map(|t| t.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        }
    }
}

#[cfg(test)]
mod tests;
