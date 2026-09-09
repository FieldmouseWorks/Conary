// apps/conary/src/commands/query/repo.rs

//! Repository query commands
//!
//! Functions for querying packages available in repositories (not installed).

use super::super::open_db;
use anyhow::Result;

/// Query packages available in repositories (not installed)
///
/// This is similar to `dnf repoquery` or `apt-cache search`.
pub fn cmd_repquery(pattern: Option<&str>, db_path: &str, info: bool) -> Result<()> {
    let conn = open_db(db_path)?;

    let packages = if let Some(pattern) = pattern {
        conary_core::db::models::RepositoryPackage::search(&conn, pattern)?
    } else {
        conary_core::db::models::RepositoryPackage::list_all(&conn)?
    };

    let repos = conary_core::db::models::Repository::list_all(&conn)?;
    if info && packages.len() == 1 {
        show_repo_package_info(&conn, &packages[0])?;
        crate::ui::repository::metadata_guidance(&repos, db_path);
    } else {
        crate::ui::repository::packages(&packages, &repos, pattern, db_path)?;
    }

    Ok(())
}

/// Show detailed info for a repository package
fn show_repo_package_info(
    conn: &rusqlite::Connection,
    pkg: &conary_core::db::models::RepositoryPackage,
) -> Result<()> {
    // Resolve every fallible fact before the renderer emits a partial frame.
    let repository = pkg.get_repository_name(conn)?;
    let mut installed = conary_core::db::models::Trove::find_by_name(conn, &pkg.name)?;
    installed.retain(|trove| trove.trove_type == conary_core::db::models::TroveType::Package);
    installed.sort_by_key(|trove| trove.id);
    let requirements =
        conary_core::db::models::RepositoryRequirementGroup::find_by_repository_package(
            conn,
            pkg.id
                .ok_or_else(|| anyhow::anyhow!("repository package has no database ID"))?,
        )?;
    crate::ui::repository::package_details(pkg, &repository, &installed, &requirements);
    Ok(())
}
