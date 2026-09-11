// crates/conary-core/src/resolver/provides_index.rs

//! Demand-driven index mapping capability names to provider packages.
//!
//! Modeled after libsolv's `pool_createwhatprovides()`. Populated on demand
//! during pre-solve candidate loading from three data sources:
//! 1. `repository_provides` (per-distro provides from repo sync)
//! 2. `provides` (installed package provides)
//! 3. `appstream_provides` (cross-distro provides from AppStream)

use crate::db::models::{ProvideEntry, RepositoryProvide};
use crate::error::Result;
use crate::repository::dependency_model::ProvideVersionRelation;
use crate::repository::versioning::{
    RepoVersionConstraint, VersionScheme, provided_range_matches_requirement,
};
use rusqlite::Connection;
use std::collections::HashMap;

/// A single provider entry in the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderEntry {
    /// Repository package ID (for repo-sourced provides)
    pub repo_package_id: Option<i64>,
    /// Installed trove ID (for locally installed provides)
    pub installed_trove_id: Option<i64>,
    /// Canonical package ID (for AppStream cross-distro provides)
    pub canonical_id: Option<i64>,
    /// Version of the provide (e.g., "3.2.0" for libssl.so.3)
    pub provide_version: Option<String>,
    /// Ordered relation associated with `provide_version`.
    pub version_relation: Option<ProvideVersionRelation>,
    /// Version comparison scheme
    pub version_scheme: Option<VersionScheme>,
}

/// Demand-driven capability-to-provider index.
///
/// The resolver asks for exact capability names while it loads a bounded
/// candidate universe. Keep the database handle so the first lookup can seek
/// each source's capability index, then cache only the capabilities this
/// resolution actually names. The cache is populated before `resolvo` starts;
/// SAT callbacks continue to use the provider's in-memory snapshot.
pub struct ProvidesIndex<'db> {
    conn: &'db Connection,
    providers: HashMap<String, Vec<ProviderEntry>>,
}

impl<'db> ProvidesIndex<'db> {
    /// Create an empty demand-driven index backed by the database connection
    /// used for this resolution.
    ///
    /// No provider rows are read here. Each later lookup is an exact
    /// capability seek and is cached for the lifetime of this resolution.
    pub fn build(conn: &'db Connection) -> Result<Self> {
        Ok(Self {
            conn,
            providers: HashMap::new(),
        })
    }

    fn load_capability(&self, capability: &str) -> Result<Vec<ProviderEntry>> {
        let mut providers = Vec::new();

        // 1. Repository provides. RepositoryProvide::find_by_capability uses
        // idx_repository_provides_capability and filters disabled repositories.
        for provide in RepositoryProvide::find_by_capability(self.conn, capability)? {
            providers.push(ProviderEntry {
                repo_package_id: Some(provide.repository_package_id),
                installed_trove_id: None,
                canonical_id: None,
                provide_version: provide.version,
                version_relation: provide.version_relation,
                version_scheme: Some(provide.version_scheme),
            });
        }

        // 2. Installed provides. The installed capability index is also
        // queried by the exact key. Unlike the former whole-table build this
        // does not join `troves`: `provides.trove_id` is NOT NULL with ON
        // DELETE CASCADE and every connection opens with foreign_keys=ON
        // (db/mod.rs open pragmas), so a provides row without its trove
        // cannot exist and the join carried no filter.
        for provide in ProvideEntry::find_all_by_capability(self.conn, capability)? {
            providers.push(ProviderEntry {
                repo_package_id: None,
                installed_trove_id: Some(provide.trove_id),
                canonical_id: None,
                provide_version: provide.version,
                version_relation: provide.version_relation,
                version_scheme: Some(provide.version_scheme),
            });
        }

        // 3. AppStream cross-distro provides. This table has its own exact
        // capability index and is intentionally bounded by the same key.
        let mut stmt = self.conn.prepare_cached(
            "SELECT ap.capability, ap.canonical_id
             FROM appstream_provides ap
             WHERE ap.capability = ?1
             ORDER BY ap.id",
        )?;
        let rows = stmt.query_map([capability], |row| {
            let capability: String = row.get(0)?;
            let canonical_id: i64 = row.get(1)?;
            Ok((capability, canonical_id))
        })?;
        for row in rows {
            // The selected capability is part of the row for parity with the
            // eager implementation; the cache key owns the grouping.
            let (_capability, canonical_id) = row?;
            providers.push(ProviderEntry {
                repo_package_id: None,
                installed_trove_id: None,
                canonical_id: Some(canonical_id),
                provide_version: None,
                version_relation: None,
                version_scheme: None,
            });
        }

        Ok(providers)
    }

    fn ensure_loaded(&mut self, capability: &str) -> Result<()> {
        if !self.providers.contains_key(capability) {
            let providers = self.load_capability(capability)?;
            self.providers.insert(capability.to_string(), providers);
        }
        Ok(())
    }

    /// Find all providers for a capability name.
    pub fn find_providers(&mut self, capability: &str) -> Result<Vec<ProviderEntry>> {
        self.ensure_loaded(capability)?;
        Ok(self.providers.get(capability).cloned().unwrap_or_default())
    }

    /// Find providers whose version satisfies a constraint.
    pub fn find_providers_constrained(
        &mut self,
        capability: &str,
        constraint: &RepoVersionConstraint,
        scheme: VersionScheme,
    ) -> Result<Vec<ProviderEntry>> {
        let mut matches = Vec::new();
        for provider in self.find_providers(capability)? {
            let matched = match provider.version_scheme {
                Some(provider_scheme) if provider_scheme == scheme => {
                    provided_range_matches_requirement(
                        scheme,
                        provider.version_relation,
                        provider.provide_version.as_deref(),
                        constraint,
                    )?
                }
                Some(_) => false,
                None => matches!(constraint, RepoVersionConstraint::Any),
            };
            if matched {
                matches.push(provider);
            }
        }
        Ok(matches)
    }

    /// Total number of unique capabilities indexed with at least one
    /// provider. Keys cached as empty by a negative lookup are not counted,
    /// matching what the former eager build would have contained.
    pub fn capability_count(&self) -> usize {
        self.providers
            .values()
            .filter(|entries| !entries.is_empty())
            .count()
    }

    /// Total number of provider entries across all capabilities.
    pub fn provider_count(&self) -> usize {
        self.providers.values().map(|v| v.len()).sum()
    }
}

#[cfg(test)]
#[path = "provides_index/tests.rs"]
mod tests;
