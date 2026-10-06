// apps/conary/src/commands/query/dependency.rs

//! Dependency query commands
//!
//! Functions for querying package dependencies, reverse dependencies,
//! what would break on removal, and what provides a capability.

use super::super::open_db;
use crate::commands::{InstalledPackageSelector, resolve_installed_package};
use anyhow::{Context, Result, ensure};
use conary_core::db::models::{
    InstalledRequirementAtom, ProvideEntry, Repository, RepositoryPackage, RepositoryProvide, Trove,
};
use conary_core::repository::dependency_model::{ProvidedCapability, RepositoryCapabilityKind};
use conary_core::repository::versioning::{VersionScheme, validate_repo_version};
use serde::Serialize;
use std::collections::HashMap;
use tracing::info;

/// Show dependencies for a package
pub fn cmd_depends(package_name: &str, db_path: &str) -> Result<()> {
    info!("Showing dependencies for package: {}", package_name);
    let conn = open_db(db_path)?;

    let trove = Trove::find_one_by_name(&conn, package_name)?
        .ok_or_else(|| anyhow::anyhow!("Package '{}' not found", package_name))?;
    let trove_id = trove.id.ok_or_else(|| anyhow::anyhow!("Trove has no ID"))?;

    let deps = InstalledRequirementAtom::find_by_trove(&conn, trove_id)?;

    if deps.is_empty() {
        println!("Package '{}' has no dependencies", package_name);
    } else {
        println!("Dependencies for package '{}':", package_name);
        for dep in deps {
            // Display typed dependency
            let typed_str = dep.to_typed_string();
            print!("  {} [{}]", typed_str, dep.dependency_type);
            if let Some(version) = dep.depends_on_version {
                print!(" - version: {}", version);
            }
            println!();
        }
    }

    Ok(())
}

/// Show reverse dependencies
pub fn cmd_rdepends(package_name: &str, db_path: &str) -> Result<()> {
    info!("Showing reverse dependencies for package: {}", package_name);
    let conn = open_db(db_path)?;

    let dependents = InstalledRequirementAtom::find_dependents(&conn, package_name)?;

    if dependents.is_empty() {
        println!(
            "No packages depend on '{}' (or package not installed)",
            package_name
        );
    } else {
        println!("Packages that depend on '{}':", package_name);
        for dep in dependents {
            if let Ok(Some(trove)) = Trove::find_by_id(&conn, dep.trove_id) {
                // Show the dependency kind if not a plain package
                let kind_str = if dep.kind != "package" && !dep.kind.is_empty() {
                    format!(" [{}]", dep.kind)
                } else {
                    String::new()
                };
                print!("  {} ({}){}", trove.name, dep.dependency_type, kind_str);
                if let Some(constraint) = dep.version_constraint {
                    print!(" - requires: {}", constraint);
                }
                println!();
            }
        }
    }

    Ok(())
}

/// Show what packages would break if a package is removed
pub fn cmd_whatbreaks(
    package_name: &str,
    db_path: &str,
    version: Option<String>,
    architecture: Option<String>,
    release: Option<crate::commands::InstalledRelease>,
) -> Result<()> {
    info!(
        "Checking what would break if '{}' is removed...",
        package_name
    );
    let conn = open_db(db_path)?;

    let selector = InstalledPackageSelector::new(package_name.to_string(), version, architecture)
        .with_release(release);
    let resolved = resolve_installed_package(&conn, &selector)?;
    let trove = resolved.trove;

    let mut has_preflight_blocker = false;
    if trove.pinned {
        println!(
            "Package '{}' is pinned and remove would be refused before mutation.",
            trove.name
        );
        has_preflight_blocker = true;
    }
    if trove.install_source.is_adopted() {
        println!(
            "Package '{}' is adopted; native package-manager authority is preserved.",
            trove.name
        );
        has_preflight_blocker = true;
    }

    // Judge the exact selected trove so a co-installed release of the same
    // name that still satisfies dependents is not counted as removed.
    let trove_id = trove
        .id
        .ok_or_else(|| anyhow::anyhow!("installed package '{}' has no trove id", trove.name))?;
    let breaking = conary_core::resolver::solve_removal_troves(&conn, &[trove_id])?;

    if breaking.is_empty() {
        if has_preflight_blocker {
            println!(
                "No dependency breakage found, but remove would still be refused before mutation."
            );
        } else {
            println!(
                "Package '{}' can be safely removed (no dependencies)",
                trove.name
            );
        }
    } else {
        println!(
            "Removing '{}' would break the following packages:",
            trove.name
        );
        for pkg in &breaking {
            println!("  {}", pkg);
        }
        println!("\nTotal: {} packages would be affected", breaking.len());
    }

    Ok(())
}

/// Find what package provides a capability
///
/// Searches for packages that provide a given capability, which can be:
/// - A package name
/// - A virtual provide (e.g., perl(DBI))
/// - A file path (e.g., /usr/bin/python3)
/// - A typed capability (e.g., soname(libssl.so.3))
pub fn cmd_whatprovides(capability: &str, db_path: &str, json: bool) -> Result<()> {
    let mut conn = open_db(db_path)?;
    let transaction = conn.transaction()?;
    let report = whatprovides_report(&transaction, capability)?;
    transaction.commit()?;

    let output = if json {
        serde_json::to_string(&report)?
    } else {
        render_whatprovides_text(&report)?
    };
    crate::ui::message(&output);
    Ok(())
}

const WHATPROVIDES_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Serialize)]
struct WhatProvidesReport {
    schema_version: u16,
    capability: String,
    providers: Vec<WhatProvidesProvider>,
    provider_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ProviderSourceKind {
    Installed,
    Repository,
}

#[derive(Debug, Serialize)]
struct WhatProvidesProvider {
    source_kind: ProviderSourceKind,
    package: ProviderPackageIdentity,
    repository: Option<ProviderRepositoryIdentity>,
    capability_versions: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ProviderPackageIdentity {
    name: String,
    version: String,
    release: Option<String>,
    architecture: Option<String>,
    version_scheme: VersionScheme,
}

#[derive(Debug, Serialize)]
struct ProviderRepositoryIdentity {
    name: String,
    repository_identity: Option<String>,
}

fn whatprovides_report(
    conn: &rusqlite::Connection,
    capability: &str,
) -> Result<WhatProvidesReport> {
    let installed_matches = installed_providers_for_capability(conn, capability)?;
    let repository_matches = repository_providers_for_capability(conn, capability)?;
    let mut providers = Vec::with_capacity(installed_matches.len() + repository_matches.len());

    for matched in installed_matches {
        let trove = Trove::find_by_id(conn, matched.trove_id)?.with_context(|| {
            format!(
                "matched installed provider references missing trove {}",
                matched.trove_id
            )
        })?;
        validate_provider_name(&trove.name, "installed trove", matched.trove_id)?;
        let mut capability_versions = matched.capability_versions;
        capability_versions.sort_unstable();
        capability_versions.dedup();
        providers.push(WhatProvidesProvider {
            source_kind: ProviderSourceKind::Installed,
            package: ProviderPackageIdentity {
                name: trove.name,
                version: trove.version,
                release: trove.package_release,
                architecture: trove.architecture,
                version_scheme: trove.version_scheme,
            },
            repository: None,
            capability_versions,
        });
    }

    for matched in repository_matches {
        let package = RepositoryPackage::find_by_id(conn, matched.repository_package_id)?
            .with_context(|| {
                format!(
                    "matched repository provider references missing package {}",
                    matched.repository_package_id
                )
            })?;
        validate_provider_name(
            &package.name,
            "repository package",
            matched.repository_package_id,
        )?;
        validate_repo_version(package.version_scheme, &package.version).with_context(|| {
            format!(
                "matched repository package {} has invalid version authority",
                matched.repository_package_id
            )
        })?;
        let repository =
            Repository::find_by_id(conn, package.repository_id)?.with_context(|| {
                format!(
                    "matched repository package {} references missing repository {}",
                    matched.repository_package_id, package.repository_id
                )
            })?;
        validate_provider_name(&repository.name, "repository", package.repository_id)?;
        let mut capability_versions = matched.capability_versions;
        capability_versions.sort_unstable();
        capability_versions.dedup();
        providers.push(WhatProvidesProvider {
            source_kind: ProviderSourceKind::Repository,
            package: ProviderPackageIdentity {
                name: package.name,
                version: package.version,
                release: (!package.package_release.is_empty()).then_some(package.package_release),
                architecture: package.architecture,
                version_scheme: package.version_scheme,
            },
            repository: Some(ProviderRepositoryIdentity {
                name: repository.name,
                repository_identity: repository.repository_identity,
            }),
            capability_versions,
        });
    }

    providers.sort_by(|left, right| provider_sort_key(left).cmp(&provider_sort_key(right)));
    let provider_count = providers.len();
    Ok(WhatProvidesReport {
        schema_version: WHATPROVIDES_SCHEMA_VERSION,
        capability: capability.to_string(),
        providers,
        provider_count,
    })
}

fn validate_provider_name(name: &str, source: &str, id: i64) -> Result<()> {
    ensure!(
        !name.trim().is_empty(),
        "matched {source} {id} has an empty name"
    );
    Ok(())
}

fn provider_sort_key(
    provider: &WhatProvidesProvider,
) -> (u8, &str, &str, &str, &str, &str, &str, &str, &[String]) {
    let source_order = match provider.source_kind {
        ProviderSourceKind::Installed => 0,
        ProviderSourceKind::Repository => 1,
    };
    (
        source_order,
        &provider.package.name,
        provider.package.version_scheme.as_str(),
        &provider.package.version,
        provider.package.release.as_deref().unwrap_or(""),
        provider.package.architecture.as_deref().unwrap_or(""),
        provider
            .repository
            .as_ref()
            .map_or("", |repository| &repository.name),
        provider
            .repository
            .as_ref()
            .and_then(|repository| repository.repository_identity.as_deref())
            .unwrap_or(""),
        &provider.capability_versions,
    )
}

fn render_whatprovides_text(report: &WhatProvidesReport) -> Result<String> {
    if report.providers.is_empty() {
        return Ok(format!("No package provides '{}'", report.capability));
    }

    let mut lines = vec![format!(
        "Capability '{}' is provided by:",
        report.capability
    )];
    for source_kind in [
        ProviderSourceKind::Installed,
        ProviderSourceKind::Repository,
    ] {
        let group = report
            .providers
            .iter()
            .filter(|provider| provider.source_kind == source_kind)
            .collect::<Vec<_>>();
        if group.is_empty() {
            continue;
        }
        lines.push(
            match source_kind {
                ProviderSourceKind::Installed => "Installed providers:",
                ProviderSourceKind::Repository => "Repository providers:",
            }
            .to_string(),
        );
        for provider in group {
            let mut line = format!("  {} {}", provider.package.name, provider.package.version);
            if matches!(source_kind, ProviderSourceKind::Installed) {
                append_capability_versions(&mut line, &provider.capability_versions);
            }
            if let Some(architecture) = &provider.package.architecture {
                line.push_str(&format!(" [{architecture}]"));
            }
            if matches!(source_kind, ProviderSourceKind::Repository) {
                let repository = provider.repository.as_ref().with_context(|| {
                    format!(
                        "repository provider '{} {}' is missing repository identity",
                        provider.package.name, provider.package.version
                    )
                })?;
                line.push_str(&format!(" @{}", repository.name));
                append_capability_versions(&mut line, &provider.capability_versions);
            }
            lines.push(line);
        }
    }
    lines.push(String::new());
    lines.push(format!("Total: {} provider(s)", report.provider_count));
    Ok(lines.join("\n"))
}

fn append_capability_versions(line: &mut String, versions: &[String]) {
    for version in versions {
        line.push_str(&format!(" (provides version: {version})"));
    }
}

#[derive(Debug)]
struct InstalledProviderMatch {
    trove_id: i64,
    capability_versions: Vec<String>,
}

#[derive(Debug)]
struct RepositoryProviderMatch {
    repository_package_id: i64,
    capability_versions: Vec<String>,
}

fn record_installed_provider(
    providers: &mut Vec<InstalledProviderMatch>,
    indexes: &mut HashMap<i64, usize>,
    provider: ProvideEntry,
) {
    let index = *indexes.entry(provider.trove_id).or_insert_with(|| {
        providers.push(InstalledProviderMatch {
            trove_id: provider.trove_id,
            capability_versions: Vec::new(),
        });
        providers.len() - 1
    });
    if let Some(version) = provider.version
        && !providers[index].capability_versions.contains(&version)
    {
        providers[index].capability_versions.push(version);
    }
}

fn record_repository_provider(
    providers: &mut Vec<RepositoryProviderMatch>,
    indexes: &mut HashMap<i64, usize>,
    provider: RepositoryProvide,
) {
    let package_id = provider.repository_package_id;
    let index = *indexes.entry(package_id).or_insert_with(|| {
        providers.push(RepositoryProviderMatch {
            repository_package_id: package_id,
            capability_versions: Vec::new(),
        });
        providers.len() - 1
    });
    if let Some(version) = provider.version
        && !providers[index].capability_versions.contains(&version)
    {
        providers[index].capability_versions.push(version);
    }
}

fn installed_providers_for_capability(
    conn: &rusqlite::Connection,
    capability: &str,
) -> Result<Vec<InstalledProviderMatch>> {
    let mut providers = Vec::new();
    let mut indexes = HashMap::new();

    for provider in ProvideEntry::find_all_by_cli_exact_query(conn, capability)? {
        validate_installed_provider(&provider)?;
        record_installed_provider(&mut providers, &mut indexes, provider);
    }

    if let Some((kind, typed_capability)) = parse_typed_capability_query(capability) {
        for provider in ProvideEntry::find_all_typed(conn, kind, typed_capability)? {
            validate_installed_provider(&provider)?;
            record_installed_provider(&mut providers, &mut indexes, provider);
        }
    }

    Ok(providers)
}

fn validate_installed_provider(provider: &ProvideEntry) -> Result<()> {
    ProvidedCapability {
        kind: provider.kind,
        name: provider.capability.clone(),
        version: provider.version.clone(),
        version_relation: provider.version_relation,
        version_scheme: provider.version_scheme,
        architecture_qualifier: provider.architecture_qualifier.clone(),
        provenance: provider.provenance.clone(),
    }
    .validate()
    .with_context(|| {
        format!(
            "invalid matched installed provide for trove {}",
            provider.trove_id
        )
    })
}

fn repository_providers_for_capability(
    conn: &rusqlite::Connection,
    capability: &str,
) -> Result<Vec<RepositoryProviderMatch>> {
    let mut providers = Vec::new();
    let mut indexes = HashMap::new();

    RepositoryProvide::validate_cli_exact_references(conn, capability)?;
    for provider in RepositoryProvide::find_by_cli_exact_query(conn, capability)? {
        provider.validated_capability().with_context(|| {
            format!(
                "invalid matched repository provide for package {}",
                provider.repository_package_id
            )
        })?;
        record_repository_provider(&mut providers, &mut indexes, provider);
    }

    if let Some((kind, typed_capability)) = parse_typed_capability_query(capability) {
        RepositoryProvide::validate_cli_typed_references(
            conn,
            typed_capability,
            capability_kind_name(kind),
        )?;
        for provider in RepositoryProvide::find_by_capability_and_kind(
            conn,
            typed_capability,
            capability_kind_name(kind),
        )? {
            provider.validated_capability().with_context(|| {
                format!(
                    "invalid matched repository provide for package {}",
                    provider.repository_package_id
                )
            })?;
            record_repository_provider(&mut providers, &mut indexes, provider);
        }
    }
    Ok(providers)
}

const fn capability_kind_name(kind: RepositoryCapabilityKind) -> &'static str {
    match kind {
        RepositoryCapabilityKind::PackageName => "package",
        RepositoryCapabilityKind::Virtual => "virtual",
        RepositoryCapabilityKind::Soname => "soname",
        RepositoryCapabilityKind::File => "file",
        RepositoryCapabilityKind::Path => "path",
        RepositoryCapabilityKind::Binary => "binary",
        RepositoryCapabilityKind::PkgConfig => "pkgconfig",
        RepositoryCapabilityKind::PkgConfig32 => "pkgconfig32",
        RepositoryCapabilityKind::Comar => "comar",
        RepositoryCapabilityKind::Generic => "generic",
    }
}

fn parse_typed_capability_query(capability: &str) -> Option<(RepositoryCapabilityKind, &str)> {
    let (kind, value) = capability.split_once('(')?;
    let value = value.strip_suffix(')')?;
    if value.is_empty() {
        return None;
    }
    let kind = match kind {
        "package" => RepositoryCapabilityKind::PackageName,
        "virtual" => RepositoryCapabilityKind::Virtual,
        "soname" => RepositoryCapabilityKind::Soname,
        "file" => RepositoryCapabilityKind::File,
        "path" => RepositoryCapabilityKind::Path,
        "binary" => RepositoryCapabilityKind::Binary,
        "pkgconfig" => RepositoryCapabilityKind::PkgConfig,
        "pkgconfig32" => RepositoryCapabilityKind::PkgConfig32,
        "comar" => RepositoryCapabilityKind::Comar,
        "generic" => RepositoryCapabilityKind::Generic,
        _ => return None,
    };
    Some((kind, value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_typed_capability_query_reads_explicit_wrapper() {
        let parsed = parse_typed_capability_query("soname(libssl.so.3)");

        assert_eq!(
            parsed,
            Some((RepositoryCapabilityKind::Soname, "libssl.so.3"))
        );
    }

    #[test]
    fn parse_typed_capability_query_ignores_native_suffix_metadata() {
        let parsed = parse_typed_capability_query("libssl.so.3()(64bit)");

        assert_eq!(parsed, None);
    }

    #[test]
    fn parse_typed_capability_query_accepts_pkgconfig32_and_comar() {
        assert_eq!(
            parse_typed_capability_query("pkgconfig32(libexample)"),
            Some((RepositoryCapabilityKind::PkgConfig32, "libexample"))
        );
        assert_eq!(
            parse_typed_capability_query("comar(system.base)"),
            Some((RepositoryCapabilityKind::Comar, "system.base"))
        );
    }
}
