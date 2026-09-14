// apps/conary/src/ui/repository/enrollment.rs
//! Durable repository enrollment and state-change results from committed commands.

use crate::commands::RepositoryOperation;
use crate::ui::transaction_summary::visible;
use crate::ui::{Status, field, heading, row};
use conary_core::db::models::Repository;

pub(crate) fn operation_complete(operation: RepositoryOperation, name: &str, database: &str) {
    heading(match operation {
        RepositoryOperation::Add => "Repository added:",
        RepositoryOperation::Enable => "Repository enabled:",
        RepositoryOperation::Disable => "Repository disabled:",
        RepositoryOperation::Remove => "Repository removed:",
        RepositoryOperation::ResetTrust => "Repository trust reset:",
    });
    field("Database", &visible(database));
    row(Status::Ok, &[&visible(name)]);
}

pub(crate) fn added(repo: &Repository, database: &str, package_key_count: usize) {
    operation_complete(RepositoryOperation::Add, &repo.name, database);
    field("Metadata URL", &visible(&repo.url));
    if let Some(content) = &repo.content_url {
        field(
            "Content URL",
            &format!("{} (reference mirror)", visible(content)),
        );
    }
    field("Enabled", &repo.enabled.to_string());
    field("Priority", &repo.priority.to_string());
    if let Some(profile) = &repo.source_profile {
        field("Source profile", &visible(profile));
    }
    if let (Some(policy), Some(identity)) = (&repo.source_policy, &repo.repository_identity) {
        field("Source identity", &visible(&policy.source_identity));
        field("Repository identity", &visible(identity));
        field("Update policy", policy.update_mode.as_str());
    }
    let trust = if let Some(policy) = &repo.trust_policy {
        super::trust_display::describe(policy)
    } else if package_key_count != 0 {
        format!("{package_key_count} pinned CCS package key(s)")
    } else {
        "typed JSON/Remi authority".into()
    };
    field("Repository trust", &visible(&trust));
    field(
        "Security advisories",
        repo.security_advisory_support.as_str(),
    );
    if let Some(strategy) = &repo.default_strategy {
        field("Default strategy", &visible(strategy));
        if strategy == "remi"
            && let Some(endpoint) = &repo.default_strategy_endpoint
        {
            field("Remi endpoint", &visible(endpoint));
        }
    }
}
