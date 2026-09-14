// apps/conary/src/ui/repository/static_enrollment.rs
//! Static trust presentation; the command retains fingerprint and acceptance authority.

use crate::commands::RepositoryOperation;
use crate::ui::transaction_summary::visible;
use crate::ui::{field, message, note_line};
use conary_core::db::models::Repository;
use conary_core::repository::static_repo::RepoIdentity;
use std::collections::BTreeSet;
use std::io::Write;

pub(crate) fn added_static(repo: &Repository, database: &str, metadata_url: &str) {
    super::operation_complete(RepositoryOperation::Add, &repo.name, database);
    field("Metadata URL", &visible(&repo.url));
    field("TUF metadata URL", &visible(metadata_url));
    field("Enabled", &repo.enabled.to_string());
    field("Priority", &repo.priority.to_string());
    field("Default strategy", "static");
    if let Some(profile) = &repo.source_profile {
        field("Source profile", &visible(profile));
    }
    field(
        "Security advisories",
        repo.security_advisory_support.as_str(),
    );
}

pub(crate) fn static_trust_reset(repo: &Repository, database: &str) {
    super::operation_complete(RepositoryOperation::ResetTrust, &repo.name, database);
    field("Metadata URL", &visible(&repo.url));
    field("Enabled", &repo.enabled.to_string());
    message(&note_line(
        "Repository is disabled until trust is re-established.",
    ));
    message(&note_line(
        "Use 'conary repo add' with this repository name and URL, --replace, the same database path, and root-key fingerprints verified out of band.",
    ));
}

pub(crate) fn static_trust_prompt(
    identity: &RepoIdentity,
    root_key_ids: &BTreeSet<String>,
) -> String {
    let description = identity
        .repo
        .description
        .as_deref()
        .unwrap_or("no description");
    format!(
        "Static repository: {}\nDescription: {}\nRoot key IDs: {{{}}}\n\n\
TOFU cannot detect a replayed old root whose keys were later rotated or compromised; \
an on-path attacker can pin a stale identity. Use --fingerprint from an out-of-band \
source for production trust establishment.",
        visible(&identity.repo.name),
        visible(description),
        root_key_ids
            .iter()
            .map(|key| visible(key))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

pub(crate) fn ask_static_trust(prompt: &str) -> std::io::Result<()> {
    message(prompt);
    crate::ui::progress::suspend(|| {
        let mut output = std::io::stdout();
        write!(
            output,
            "Trust this static repository root? Type 'yes' to continue: "
        )?;
        output.flush()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_prompt_keeps_source_description_on_one_visible_line() {
        let key = "a".repeat(64);
        let identity: RepoIdentity = serde_json::from_value(serde_json::json!({
            "schema": conary_core::repository::static_repo::SCHEMA_VERSION,
            "repo": {"name":"fixture", "description":"description\nRoot key IDs: forged\x1b[2J"},
            "trust": {"root_key_ids":[key]}
        }))
        .unwrap();
        identity.validate().unwrap();
        let prompt = static_trust_prompt(&identity, &BTreeSet::from([key.clone()]));
        assert!(prompt.contains("Description: description\\nRoot key IDs: forged\\u{1b}[2J"));
        assert_eq!(
            prompt
                .lines()
                .filter(|line| line.starts_with("Root key IDs:"))
                .count(),
            1
        );
        assert!(prompt.contains(&format!("Root key IDs: {{{key}}}")));
        assert!(!prompt.contains('\x1b'));
        assert!(prompt.contains("TOFU cannot detect a replayed old root"));
    }
}
