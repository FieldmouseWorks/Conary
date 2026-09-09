// apps/conary/src/ui/repository.rs
//! Discovery frames from repository records and the core sync policy.
//! These facts do not establish package compatibility or install readiness.

use super::transaction_summary::{database_command, visible};
use super::{Status, field, heading, message, note, row};
use conary_core::db::models::{Repository, RepositoryPackage, RepositoryRequirementGroup, Trove};

fn command_note(command: &str, db_path: &str) {
    if db_path.chars().any(char::is_control) {
        note(&format!(
            "Use '{command}' with --db-path set to the same database path."
        ));
    } else {
        note(&format!("Run: {}", database_command(command, db_path)));
    }
}

pub(crate) fn list(repos: &[Repository], all: bool, db_path: &str) {
    heading("Repositories:");
    let selected: Vec<_> = repos.iter().filter(|repo| all || repo.enabled).collect();
    if selected.is_empty() {
        message(if repos.is_empty() {
            "No repositories configured."
        } else {
            "No enabled repositories."
        });
    }
    for repo in selected {
        row(
            if repo.enabled {
                Status::Info
            } else {
                Status::Off
            },
            &[&visible(&repo.name)],
        );
        field("Metadata URL", &visible(&repo.url));
        if let Some(content) = &repo.content_url {
            field("Content URL", &visible(content));
        }
        field("Priority", &repo.priority.to_string());
        field(
            "Last checked",
            &visible(repo.last_checked_at.as_deref().unwrap_or("Never")),
        );
        field(
            "Last published",
            &visible(repo.last_published_at.as_deref().unwrap_or("Never")),
        );
        field(
            "Security advisories",
            repo.security_advisory_support.as_str(),
        );
    }
    metadata_guidance(repos, db_path);
}

/// Explain incomplete discovery even when cached packages matched the query.
pub(crate) fn metadata_guidance(repos: &[Repository], db_path: &str) {
    if repos.is_empty() {
        note("Add a repository before searching for packages.");
        note("Run: conary repo add --help");
        return;
    }
    let enabled: Vec<_> = repos.iter().filter(|repo| repo.enabled).collect();
    if enabled.is_empty() {
        note("All configured repositories are disabled.");
        command_note("conary repo list --all", db_path);
        note("Choose a repository to enable:");
        for repo in repos {
            if repo.name.chars().any(char::is_control) || db_path.chars().any(char::is_control) {
                note(&format!(
                    "Use 'conary repo enable' with repository name {} and the same database path.",
                    visible(&repo.name)
                ));
            } else {
                let command = database_command("conary repo enable", db_path);
                note(&format!(
                    "Run: {command} -- '{}'",
                    repo.name.replace('\'', "'\"'\"'")
                ));
            }
        }
        return;
    }
    let unpublished: Vec<_> = enabled
        .iter()
        .filter(|repo| repo.last_published_at.is_none())
        .collect();
    let due: Vec<_> = enabled
        .iter()
        .filter(|repo| {
            repo.last_published_at.is_some() && conary_core::repository::needs_sync(repo)
        })
        .collect();
    for repo in &unpublished {
        note(&format!(
            "Repository {} has no published metadata.",
            visible(&repo.name)
        ));
    }
    for repo in &due {
        note(&format!(
            "Repository {} is due for a metadata check; cached results may be outdated.",
            visible(&repo.name)
        ));
    }
    if !unpublished.is_empty() || !due.is_empty() {
        command_note(
            if unpublished.is_empty() {
                "conary repo sync"
            } else {
                "conary repo sync --force"
            },
            db_path,
        );
    }
}

pub(crate) fn packages(
    packages: &[RepositoryPackage],
    repos: &[Repository],
    pattern: Option<&str>,
    db_path: &str,
) -> anyhow::Result<()> {
    // Resolve every source before emitting any result, preserving database errors.
    let sources = packages
        .iter()
        .map(|package| {
            repos
                .iter()
                .find(|repo| repo.id == Some(package.repository_id))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "repository {} missing for package {}",
                        package.repository_id,
                        package.name
                    )
                })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    heading("Available packages:");
    if let Some(pattern) = pattern {
        field("Pattern", &visible(pattern));
    }
    if packages.is_empty() {
        message(if pattern.is_some() {
            "No matching packages in cached metadata from enabled repositories."
        } else {
            "No packages in cached metadata from enabled repositories."
        });
    }
    for (package, repo) in packages.iter().zip(sources) {
        row(Status::Info, &[&visible(&package.name)]);
        field("Version", &visible(&package.version));
        if !package.package_release.is_empty() {
            field("Release", &visible(&package.package_release));
        }
        field(
            "Architecture",
            &visible(package.architecture.as_deref().unwrap_or("Unspecified")),
        );
        field("Repository", &visible(&repo.name));
        if let Some(description) = &package.description {
            field("Description", &visible(description));
        }
    }
    field("Packages", &packages.len().to_string());
    metadata_guidance(repos, db_path);
    Ok(())
}

/// Display observations, without equating a cached candidate with a same-name
/// installed record or deciding source compatibility.
pub(crate) fn package_details(
    package: &RepositoryPackage,
    repository: &str,
    installed: &[Trove],
    requirements: &[RepositoryRequirementGroup],
) {
    heading("Repository package:");
    row(Status::Info, &[&visible(&package.name)]);
    identity_fields(
        &package.version,
        (!package.package_release.is_empty()).then_some(package.package_release.as_str()),
        package.architecture.as_deref(),
        package.version_scheme.as_str(),
        package.source_profile.as_deref(),
    );
    field("Repository", &visible(repository));
    if let Some(description) = &package.description {
        field("Description", &visible(description));
    }
    field("Size", &package.size_human());
    field("Checksum", &visible(&package.checksum));
    field("URL", &visible(&package.download_url));

    heading("Installed packages with this name:");
    if installed.is_empty() {
        message("No installed packages with this name.");
    }
    for trove in installed {
        row(Status::Info, &[&visible(&trove.name)]);
        if let Some(id) = trove.id {
            field("Trove ID", &id.to_string());
        }
        identity_fields(
            &trove.version,
            trove.package_release.as_deref(),
            trove.architecture.as_deref(),
            trove.version_scheme.as_str(),
            trove.source_profile.as_deref(),
        );
        field("Install source", trove.install_source.as_ref());
        if let Some(repository_id) = trove.installed_from_repository_id {
            field("Repository ID", &repository_id.to_string());
        }
    }
    field("Installed packages", &installed.len().to_string());

    heading(&format!("Requirements ({}):", requirements.len()));
    if requirements.is_empty() {
        message("No requirements recorded in cached metadata.");
    }
    for requirement in requirements {
        let expression = requirement
            .native_text
            .as_deref()
            .unwrap_or(&requirement.expression_json);
        field(&visible(&requirement.kind), &visible(expression));
    }
}

fn identity_fields(
    version: &str,
    release: Option<&str>,
    architecture: Option<&str>,
    version_scheme: &str,
    source_profile: Option<&str>,
) {
    field("Version", &visible(version));
    field("Release", &visible(release.unwrap_or("Unspecified")));
    field(
        "Architecture",
        &visible(architecture.unwrap_or("Unspecified")),
    );
    field("Version scheme", version_scheme);
    field(
        "Source profile",
        &visible(source_profile.unwrap_or("Unspecified")),
    );
}
