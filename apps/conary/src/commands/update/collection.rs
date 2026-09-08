// apps/conary/src/commands/update/collection.rs

//! Collection update orchestration for `conary update @collection`.

use super::super::install::OwnershipMode;
use super::super::{SandboxMode, open_db};
use super::adopted_authority::{
    AdoptedUpdateDecision, adopted_update_decision, native_manager_for_trove,
};
use super::outcome::{
    CollectionSelectionEntry, CollectionSelectionStatus, CollectionUpdateEntry,
    CollectionUpdateStatus,
};
use super::package::update_packages;
use super::selection::{
    SecurityMetadataUnavailable, UpdateCandidateSelection, print_security_metadata_unavailable,
    security_metadata_unavailable_error, select_update_candidate,
};
use anyhow::Result;
use conary_core::db::models::{CollectionMember, Trove, TroveType};
use conary_core::repository::resolution_policy::RequestScope;
use tracing::info;

#[derive(Debug, Clone, PartialEq, Eq)]
struct CollectionUpdateTarget {
    name: String,
    version: String,
    architecture: Option<String>,
}

impl CollectionUpdateTarget {
    fn from_trove(trove: &Trove) -> Self {
        Self {
            name: trove.name.clone(),
            version: trove.version.clone(),
            architecture: trove.architecture.clone(),
        }
    }

    fn display(&self) -> String {
        match self.architecture.as_deref() {
            Some(arch) => format!("{} {} [{}]", self.name, self.version, arch),
            None => format!("{} {}", self.name, self.version),
        }
    }
}

/// Update all members of a collection/group (best-effort, per-package)
///
/// This updates all installed packages that are members of the specified collection.
/// Updates are applied one package at a time; earlier members remain updated even if
/// a later one fails.  Returns an error if any member fails to update.
/// If `security_only` is true, only applies security updates.
#[allow(clippy::too_many_arguments)]
pub async fn cmd_update_group(
    name: &str,
    db_path: &str,
    root: &str,
    security_only: bool,
    dry_run: bool,
    sandbox_mode: SandboxMode,
    ownership: Option<OwnershipMode>,
    yes: bool,
) -> Result<()> {
    info!("Updating collection: {}", name);
    let requested_ownership = ownership;
    let effective_ownership = requested_ownership.unwrap_or_default();
    let conn = open_db(db_path)?;
    let effective_source_policy =
        conary_core::repository::load_effective_policy(&conn, RequestScope::Any)?;
    let policy = effective_source_policy.resolution;

    let troves = Trove::find_by_name(&conn, name)?;
    let collection = troves
        .iter()
        .find(|t| t.trove_type == TroveType::Collection)
        .ok_or_else(|| anyhow::anyhow!("Collection '{}' not found", name))?;

    let collection_id = collection
        .id
        .ok_or_else(|| anyhow::anyhow!("Collection has no ID"))?;
    let members = CollectionMember::find_by_collection(&conn, collection_id)?;

    if members.is_empty() {
        crate::ui::update_summary::collection_selection_summary(name, 0, security_only, &[]);
        return Ok(());
    }

    // Find installed members that need updates
    let mut updates_to_apply: Vec<CollectionUpdateTarget> = Vec::new();
    let mut selection = Vec::new();
    let mut security_metadata_unavailable: Vec<SecurityMetadataUnavailable> = Vec::new();

    for member in &members {
        let installed = Trove::find_by_name(&conn, &member.member_name)?
            .into_iter()
            .filter(|trove| trove.trove_type == TroveType::Package)
            .collect::<Vec<_>>();
        if installed.is_empty() {
            selection.push(CollectionSelectionEntry {
                target: member.member_name.clone(),
                status: CollectionSelectionStatus::NotInstalled,
            });
            continue;
        }

        for trove in &installed {
            if trove.pinned {
                selection.push(CollectionSelectionEntry {
                    target: CollectionUpdateTarget::from_trove(trove).display(),
                    status: CollectionSelectionStatus::Pinned,
                });
                continue;
            }

            let adopted_decision = if trove.install_source.is_adopted() {
                Some(adopted_update_decision(
                    effective_ownership,
                    requested_ownership,
                ))
            } else {
                None
            };

            if trove.install_source.is_adopted() {
                let native_manager = native_manager_for_trove(trove);
                match adopted_decision.expect("adopted trove must have an update decision") {
                    AdoptedUpdateDecision::QueueTakeover => {}
                    AdoptedUpdateDecision::SkipNativeAuthority => {
                        let guidance = native_manager.map_or_else(
                            || "the recorded external owner".to_string(),
                            |manager| manager.update_command(&trove.name),
                        );
                        selection.push(CollectionSelectionEntry {
                            target: CollectionUpdateTarget::from_trove(trove).display(),
                            status: CollectionSelectionStatus::ExternallyManaged { guidance },
                        });
                        continue;
                    }
                }
            }

            let enforce_security_metadata = security_only
                && !matches!(
                    adopted_decision,
                    Some(AdoptedUpdateDecision::SkipNativeAuthority)
                );
            match select_update_candidate(&conn, trove, enforce_security_metadata, &policy)? {
                UpdateCandidateSelection::Selected(_) => {
                    let target = CollectionUpdateTarget::from_trove(trove);
                    selection.push(CollectionSelectionEntry {
                        target: target.display(),
                        status: CollectionSelectionStatus::Selected,
                    });
                    updates_to_apply.push(target);
                }
                UpdateCandidateSelection::NoEligibleUpdate => {
                    selection.push(CollectionSelectionEntry {
                        target: CollectionUpdateTarget::from_trove(trove).display(),
                        status: CollectionSelectionStatus::NoEligibleUpdate,
                    });
                }
                UpdateCandidateSelection::SecurityMetadataUnavailable(unavailable) => {
                    security_metadata_unavailable.push(unavailable);
                }
            }
        }
    }

    drop(conn);

    if !security_metadata_unavailable.is_empty() {
        print_security_metadata_unavailable(&security_metadata_unavailable);
        anyhow::bail!(security_metadata_unavailable_error(
            security_metadata_unavailable.len()
        ));
    }

    crate::ui::update_summary::collection_selection_summary(
        name,
        members.len(),
        security_only,
        &selection,
    );
    if updates_to_apply.is_empty() {
        return Ok(());
    }

    crate::ui::println!(
        "{} {} package request(s) from collection '{}':",
        if dry_run { "Previewing" } else { "Updating" },
        updates_to_apply.len(),
        name
    );
    // Update each package
    let mut outcomes = Vec::with_capacity(updates_to_apply.len());
    let mut failed_count = 0;

    for target in &updates_to_apply {
        crate::ui::println!(
            "\n{} {}...",
            if dry_run { "Previewing" } else { "Updating" },
            target.display()
        );
        let status = match update_packages(
            Some(target.name.clone()),
            db_path,
            root,
            security_only,
            dry_run,
            sandbox_mode,
            requested_ownership,
            yes,
            Some(target.version.clone()),
            target.architecture.clone(),
        )
        .await
        {
            Ok(outcome) => CollectionUpdateStatus::Completed(outcome),
            Err(error) => {
                crate::ui::diagnostics::report_error(&error);
                failed_count += 1;
                CollectionUpdateStatus::Failed
            }
        };
        outcomes.push(CollectionUpdateEntry {
            target: target.display(),
            status,
        });
    }

    crate::ui::update_summary::collection_update_summary(name, dry_run, &outcomes);
    if failed_count > 0 {
        return Err(anyhow::anyhow!(
            "{} of {} update request(s) in collection '{}' failed",
            failed_count,
            updates_to_apply.len(),
            name
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::SandboxMode;
    use crate::commands::test_helpers::create_test_db;
    use conary_core::db::models::{
        CollectionMember, InstallSource, Repository, RepositoryPackage, Trove, TroveType,
    };

    #[tokio::test]
    async fn collection_update_preserves_member_variant_selector() {
        let (_temp, db_path) = create_test_db();
        let conn = conary_core::db::open(&db_path).unwrap();

        let mut repo = Repository::new(
            "variant-repo".to_string(),
            "https://example.test/variant".to_string(),
        );
        repo.source_profile = Some("solus".to_string());
        let repo_id = repo.insert(&conn).unwrap();

        let mut collection = Trove::new(
            "base".to_string(),
            "1.0.0".to_string(),
            TroveType::Collection,
            conary_core::repository::versioning::VersionScheme::Conary,
        );
        let collection_id = collection.insert(&conn).unwrap();
        CollectionMember::new(collection_id, "demo".to_string())
            .insert(&conn)
            .unwrap();

        for arch in ["x86_64", "aarch64"] {
            let mut installed = Trove::new_with_source(
                "demo".to_string(),
                "1.0-1".to_string(),
                TroveType::Package,
                InstallSource::Repository,
                conary_core::repository::versioning::VersionScheme::Eopkg,
            );
            installed.architecture = Some(arch.to_string());
            installed.source_profile = Some("solus".to_string());
            installed.installed_from_repository_id = Some(repo_id);
            installed.insert(&conn).unwrap();

            let mut candidate = RepositoryPackage::new(
                repo_id,
                "demo".to_string(),
                "1.0-2".to_string(),
                conary_core::repository::versioning::VersionScheme::Eopkg,
                format!("sha256:demo-{arch}"),
                123,
                format!("https://example.test/variant/demo-1.0.1-{arch}.ccs"),
            );
            candidate.architecture = Some(arch.to_string());
            candidate.source_profile = Some("solus".to_string());
            candidate.insert(&conn).unwrap();
        }
        drop(conn);

        let result = cmd_update_group(
            "base",
            &db_path,
            "/",
            false,
            true,
            SandboxMode::Always,
            None,
            true,
        )
        .await;

        assert!(
            result.is_ok(),
            "collection update should preserve member variant selectors: {:?}",
            result
        );

        let planned = update_packages(
            Some("demo".into()),
            &db_path,
            "/",
            false,
            true,
            SandboxMode::Always,
            None,
            true,
            Some("1.0-1".into()),
            Some("x86_64".into()),
        )
        .await
        .unwrap();
        assert_eq!(
            planned,
            super::super::outcome::UpdateOutcome::Planned { packages: 1 }
        );
        let conn = conary_core::db::open(&db_path).unwrap();
        conn.execute("DELETE FROM repository_packages", []).unwrap();
        drop(conn);
        let unchanged = update_packages(
            Some("demo".into()),
            &db_path,
            "/",
            false,
            false,
            SandboxMode::Always,
            None,
            true,
            Some("1.0-1".into()),
            Some("x86_64".into()),
        )
        .await
        .unwrap();
        assert_eq!(unchanged, super::super::outcome::UpdateOutcome::NoChanges);
    }
}
