// apps/conary/src/commands/query/package.rs

//! Package query commands
//!
//! Functions for querying installed packages, showing package info,
//! and listing package files.

use super::super::open_db;
use super::QueryOptions;
use crate::commands::{
    InstalledPackageSelector, package_authority_label, resolve_installed_package,
};
use anyhow::Result;

/// Query installed packages
pub fn cmd_query(pattern: Option<&str>, db_path: &str, options: QueryOptions) -> Result<()> {
    let conn = open_db(db_path)?;

    // Path query mode: find package containing a file
    if let Some(file_path) = &options.path {
        if options.version.is_some() || options.architecture.is_some() || options.release.is_some()
        {
            anyhow::bail!(
                "Installed package selectors --version/--release/--arch cannot be used with --path"
            );
        }
        return query_by_path(&conn, file_path, &options);
    }

    if options.info || options.files || options.lsl {
        let package_name = pattern.ok_or_else(|| {
            anyhow::anyhow!("A package name is required with --info, --files, or --lsl")
        })?;
        let selector = InstalledPackageSelector::new(
            package_name.to_string(),
            options.version.clone(),
            options.architecture.clone(),
        )
        .with_release(options.release.clone());
        let resolved = resolve_installed_package(&conn, &selector)?;

        if options.info {
            return show_package_info(&conn, &resolved.trove, &options);
        }
        return list_package_files(&conn, &resolved.trove, options.lsl);
    }

    if options.version.is_some() || options.architecture.is_some() || options.release.is_some() {
        let package_name = pattern.ok_or_else(|| {
            anyhow::anyhow!("A package name is required with --version, --release, or --arch")
        })?;
        let selector = InstalledPackageSelector::new(
            package_name.to_string(),
            options.version.clone(),
            options.architecture.clone(),
        )
        .with_release(options.release.clone());
        let resolved = resolve_installed_package(&conn, &selector)?;
        crate::ui::installed_list::list(&[resolved.trove], Some(package_name), db_path);
        return Ok(());
    }

    let troves = if let Some(pattern) = pattern {
        conary_core::db::models::Trove::find_by_name(&conn, pattern)?
    } else {
        conary_core::db::models::Trove::list_all(&conn)?
    };

    crate::ui::installed_list::list(&troves, pattern, db_path);

    Ok(())
}

fn installed_release_label(trove: &conary_core::db::models::Trove) -> &str {
    trove.package_release.as_deref().unwrap_or("Unspecified")
}

/// Query package by file path
fn query_by_path(
    conn: &rusqlite::Connection,
    file_path: &str,
    options: &QueryOptions,
) -> Result<()> {
    // Try exact match first
    let file = conary_core::db::models::FileEntry::find_by_path(conn, file_path)?;

    if let Some(file) = file
        && let Some(trove) = conary_core::db::models::Trove::find_by_id(conn, file.trove_id)?
    {
        if options.info {
            return show_package_info(conn, &trove, options);
        }
        println!("{} {} provides:", trove.name, trove.version);
        println!("  {}", file_path);
        return Ok(());
    }

    // Try pattern match
    let pattern = if file_path.contains('%') || file_path.contains('*') {
        file_path.replace('*', "%")
    } else {
        format!("%{file_path}%")
    };

    let files = conary_core::db::models::FileEntry::find_by_path_pattern(conn, &pattern)?;
    if files.is_empty() {
        println!("No package owns file matching '{}'", file_path);
        return Ok(());
    }

    // Group by trove
    let mut trove_files: std::collections::HashMap<i64, Vec<String>> =
        std::collections::HashMap::new();
    for file in &files {
        trove_files
            .entry(file.trove_id)
            .or_default()
            .push(file.path.clone());
    }

    println!("Packages owning files matching '{}':", file_path);
    for (trove_id, paths) in &trove_files {
        if let Some(trove) = conary_core::db::models::Trove::find_by_id(conn, *trove_id)? {
            println!("\n{} {}:", trove.name, trove.version);
            for path in paths {
                println!("  {}", path);
            }
        }
    }

    Ok(())
}

/// Show detailed package information
fn show_package_info(
    conn: &rusqlite::Connection,
    trove: &conary_core::db::models::Trove,
    _options: &QueryOptions,
) -> Result<()> {
    let trove_id = trove.id.ok_or_else(|| anyhow::anyhow!("Trove has no ID"))?;

    // Resolve every fallible observation before emitting the detail frame.
    let repository = trove
        .installed_from_repository_id
        .map(|id| repository_display_name(conn, id))
        .transpose()?;
    let payload = conary_core::db::models::PackagePayloadOwnership::load(conn, trove_id)?;
    let dependencies =
        conary_core::db::models::InstalledRequirementAtom::find_by_trove(conn, trove_id)?;
    let mut provides = conary_core::db::models::ProvideEntry::find_by_trove(conn, trove_id)?;
    // Persisted IDs establish display order without sorting shared resolver reads.
    provides.sort_by_key(|provide| provide.id);
    let components = conary_core::db::models::Component::find_by_trove(conn, trove_id)?;
    let payload_bytes = payload
        .entries()
        .iter()
        .filter_map(|file| file.content.as_ref().map(|content| content.size))
        .sum();
    crate::ui::installed_info::details(&crate::ui::installed_info::InstalledPackageInfo {
        trove,
        authority: package_authority_label(trove.install_source.clone()),
        repository: repository.as_deref(),
        file_records: payload.entries().len(),
        payload_bytes,
        dependencies: &dependencies,
        provides: &provides,
        components: &components,
    });

    Ok(())
}

fn repository_display_name(conn: &rusqlite::Connection, repository_id: i64) -> Result<String> {
    let name = conary_core::db::models::Repository::find_by_id(conn, repository_id)?
        .map(|repo| repo.name)
        .unwrap_or_else(|| repository_id.to_string());
    Ok(name)
}

/// List package files
fn list_package_files(
    conn: &rusqlite::Connection,
    trove: &conary_core::db::models::Trove,
    lsl: bool,
) -> Result<()> {
    let trove_id = trove.id.ok_or_else(|| anyhow::anyhow!("Trove has no ID"))?;
    let payload = conary_core::db::models::PackagePayloadOwnership::load(conn, trove_id)?;
    let files = payload.entries();

    if files.is_empty() {
        println!("No files in package {} {}", trove.name, trove.version);
        return Ok(());
    }

    println!(
        "Files in {} {} release={} ({} files):",
        trove.name,
        trove.version,
        installed_release_label(trove),
        files.len()
    );

    if lsl {
        // ls -l style output
        for file in files {
            println!(
                "{} {:>8} {:>8} {:>8} {}",
                file.format_permissions(),
                display_payload_identity(&file.node.source.user),
                display_payload_identity(&file.node.source.group),
                file.size_human(),
                file.path
            );
        }
    } else {
        // Simple list
        for file in files {
            println!("{}", file.path);
        }
    }

    Ok(())
}

fn display_payload_identity(identity: &conary_core::payload::PayloadIdentity) -> String {
    match identity {
        conary_core::payload::PayloadIdentity::Numeric { id } => id.to_string(),
        conary_core::payload::PayloadIdentity::Named { name } => name.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conary_core::db::schema;
    use rusqlite::Connection;

    fn test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        schema::ensure_current(&conn).unwrap();
        conn
    }

    #[test]
    fn repository_display_name_prefers_repository_name() {
        let conn = test_db();
        conn.execute(
            "INSERT INTO repositories (name, url, enabled, priority)
             VALUES ('fedora-remi', 'https://remi.example.test', 1, 10)",
            [],
        )
        .unwrap();
        let repo_id = conn.last_insert_rowid();

        assert_eq!(
            repository_display_name(&conn, repo_id).unwrap(),
            "fedora-remi"
        );
    }

    #[test]
    fn repository_display_name_falls_back_to_id_for_stale_rows() {
        let conn = test_db();

        assert_eq!(repository_display_name(&conn, 99).unwrap(), "99");
    }
}
