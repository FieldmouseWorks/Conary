// apps/conary/src/commands/collection.rs
//! Collection management commands

use super::open_db;
use anyhow::{Context, Result};
use conary_core::scriptlet::SandboxMode;
use rusqlite::Connection;
use tracing::info;

/// Version stored on locally created collection troves.
///
/// Collections carry no upstream version of their own; this grammar-valid
/// placeholder satisfies the Conary version scheme that [`Trove::insert`]
/// validates against.
///
/// [`Trove::insert`]: conary_core::db::models::Trove::insert
pub const COLLECTION_TROVE_VERSION: &str = "1.0.0";

/// Find a collection trove by name and return its database ID.
fn find_collection_id(conn: &Connection, name: &str) -> Result<i64> {
    let troves = conary_core::db::models::Trove::find_by_name(conn, name)?;
    let trove = troves
        .iter()
        .find(|t| t.trove_type == conary_core::db::models::TroveType::Collection)
        .ok_or_else(|| anyhow::anyhow!("Collection '{}' not found", name))?;
    trove
        .id
        .ok_or_else(|| anyhow::anyhow!("Collection has no ID"))
}

/// Like `find_collection_id` but returns `conary_core::Result` for use inside transactions.
fn find_collection_id_core(conn: &Connection, name: &str) -> conary_core::Result<i64> {
    let troves = conary_core::db::models::Trove::find_by_name(conn, name)?;
    let trove = troves
        .iter()
        .find(|t| t.trove_type == conary_core::db::models::TroveType::Collection)
        .ok_or_else(|| conary_core::Error::NotFound(format!("Collection '{}' not found", name)))?;
    trove
        .id
        .ok_or_else(|| conary_core::Error::NotFound("Collection has no ID".to_string()))
}

/// Create a new collection
pub fn cmd_collection_create(
    name: &str,
    description: Option<&str>,
    members: &[String],
    db_path: &str,
) -> Result<()> {
    info!("Creating collection: {}", name);
    let mut conn = open_db(db_path)?;

    // Check if collection already exists
    let existing = conary_core::db::models::Trove::find_by_name(&conn, name)?;
    if !existing.is_empty() {
        return Err(anyhow::anyhow!(
            "A package or collection named '{}' already exists",
            name
        ));
    }

    conary_core::db::transaction(&mut conn, |tx| {
        // Create the collection as a trove
        let mut trove = conary_core::db::models::Trove::new(
            name.to_string(),
            COLLECTION_TROVE_VERSION.to_string(),
            conary_core::db::models::TroveType::Collection,
            conary_core::repository::versioning::VersionScheme::Conary,
        );
        trove.description = description.map(|s| s.to_string());
        let collection_id = trove.insert(tx)?;

        // Add members
        for member_name in members {
            let mut member =
                conary_core::db::models::CollectionMember::new(collection_id, member_name.clone());
            member.insert(tx)?;
        }

        Ok(())
    })?;

    println!("Created collection: {}", name);
    if let Some(desc) = description {
        println!("  Description: {}", desc);
    }
    if !members.is_empty() {
        println!("  Members: {}", members.join(", "));
    }

    Ok(())
}

/// List all collections
pub fn cmd_collection_list(db_path: &str) -> Result<()> {
    let conn = open_db(db_path)?;

    // Find all troves with type 'collection'
    let mut stmt = conn.prepare(
        "SELECT id, name, version, description FROM troves WHERE type = 'collection' ORDER BY name",
    )?;

    let collections: Vec<(i64, String, String, Option<String>)> = stmt
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("Failed to list collections")?;

    if collections.is_empty() {
        println!("No collections found.");
        println!(
            "\nCreate a collection with: conary collection-create <name> --members pkg1,pkg2,..."
        );
        return Ok(());
    }

    println!("Collections:");
    for (id, name, version, description) in &collections {
        let members = conary_core::db::models::CollectionMember::find_by_collection(&conn, *id)?;
        print!("  {} v{}", name, version);
        if let Some(desc) = description {
            print!(" - {}", desc);
        }
        println!(" ({} members)", members.len());
    }

    println!("\nTotal: {} collection(s)", collections.len());
    Ok(())
}

/// Show details of a collection
pub fn cmd_collection_show(name: &str, db_path: &str) -> Result<()> {
    let conn = open_db(db_path)?;

    let troves = conary_core::db::models::Trove::find_by_name(&conn, name)?;
    let trove = troves
        .iter()
        .find(|t| t.trove_type == conary_core::db::models::TroveType::Collection)
        .ok_or_else(|| anyhow::anyhow!("Collection '{}' not found", name))?;

    let collection_id = trove
        .id
        .ok_or_else(|| anyhow::anyhow!("Collection has no ID"))?;
    let members =
        conary_core::db::models::CollectionMember::find_by_collection(&conn, collection_id)?;

    println!("Collection: {} v{}", trove.name, trove.version);
    if let Some(desc) = &trove.description {
        println!("Description: {}", desc);
    }
    println!("\nMembers ({}):", members.len());

    for member in &members {
        print!("  {}", member.member_name);
        if let Some(ver) = &member.member_version {
            print!(" ({})", ver);
        }
        if member.is_optional {
            print!(" [optional]");
        }

        // Check if member is installed
        let installed = conary_core::db::models::Trove::find_by_name(&conn, &member.member_name)?;
        if installed.is_empty() {
            print!(" [not installed]");
        } else {
            print!(" [installed: {}]", installed[0].version);
        }
        println!();
    }

    Ok(())
}

/// Add members to a collection
pub fn cmd_collection_add(name: &str, members: &[String], db_path: &str) -> Result<()> {
    info!("Adding members to collection: {}", name);
    let mut conn = open_db(db_path)?;

    conary_core::db::transaction(&mut conn, |tx| {
        let collection_id = find_collection_id_core(tx, name)?;
        for member_name in members {
            // Check if already a member
            if conary_core::db::models::CollectionMember::is_member(tx, collection_id, member_name)?
            {
                println!("  {} is already a member, skipping", member_name);
                continue;
            }
            let mut member =
                conary_core::db::models::CollectionMember::new(collection_id, member_name.clone());
            member.insert(tx)?;
            println!("  Added: {}", member_name);
        }
        Ok(())
    })?;

    println!("\nUpdated collection '{}'", name);
    Ok(())
}

/// Remove members from a collection
pub fn cmd_collection_remove_member(name: &str, members: &[String], db_path: &str) -> Result<()> {
    info!("Removing members from collection: {}", name);
    let mut conn = open_db(db_path)?;

    conary_core::db::transaction(&mut conn, |tx| {
        let collection_id = find_collection_id_core(tx, name)?;
        for member_name in members {
            if let Some(member) = conary_core::db::models::CollectionMember::find_member(
                tx,
                collection_id,
                member_name,
            )? {
                if let Some(id) = member.id {
                    conary_core::db::models::CollectionMember::delete(tx, id)?;
                    println!("  Removed: {}", member_name);
                }
            } else {
                println!("  {} is not a member, skipping", member_name);
            }
        }
        Ok(())
    })?;

    println!("\nUpdated collection '{}'", name);
    Ok(())
}

/// Delete a collection
pub fn cmd_collection_delete(name: &str, db_path: &str) -> Result<()> {
    info!("Deleting collection: {}", name);
    let mut conn = open_db(db_path)?;

    conary_core::db::transaction(&mut conn, |tx| {
        let collection_id = find_collection_id_core(tx, name)?;
        conary_core::db::models::Trove::delete(tx, collection_id)?;
        Ok(())
    })?;

    println!("Deleted collection: {}", name);
    Ok(())
}

/// Install all packages in a collection
#[allow(clippy::too_many_arguments)]
pub async fn cmd_collection_install(
    name: &str,
    db_path: &str,
    root: &str,
    dry_run: bool,
    skip_optional: bool,
    sandbox_mode: SandboxMode,
) -> Result<()> {
    info!("Installing collection: {}", name);
    let conn = open_db(db_path)?;
    let collection_id = find_collection_id(&conn, name)?;
    let members =
        conary_core::db::models::CollectionMember::find_by_collection(&conn, collection_id)?;

    if members.is_empty() {
        println!("Collection '{}' has no members.", name);
        return Ok(());
    }

    // Filter out optional members if requested
    let members_to_install: Vec<_> = members
        .iter()
        .filter(|m| !skip_optional || !m.is_optional)
        .collect();

    println!(
        "Collection '{}' contains {} package(s) to install:",
        name,
        members_to_install.len()
    );
    for member in &members_to_install {
        print!("  {}", member.member_name);
        if member.is_optional {
            print!(" [optional]");
        }
        // Check if already installed
        let installed = conary_core::db::models::Trove::find_by_name(&conn, &member.member_name)?;
        if !installed.is_empty() {
            print!(" (already installed: {})", installed[0].version);
        }
        println!();
    }

    if dry_run {
        println!("\nDry run - no packages will be installed.");
        return Ok(());
    }

    // Drop the connection before calling cmd_install
    drop(conn);

    // Install each member that isn't already installed
    let mut installed_count = 0;
    let mut skipped_count = 0;
    let mut failed_count = 0;

    for member in &members_to_install {
        // Re-check if installed (connection was dropped)
        let conn = open_db(db_path)?;
        let installed = conary_core::db::models::Trove::find_by_name(&conn, &member.member_name)?;
        drop(conn);

        if !installed.is_empty() {
            skipped_count += 1;
            continue;
        }

        println!("\nInstalling {}...", member.member_name);
        let reason = format!("Installed via @{}", name);
        match super::cmd_install(
            &member.member_name,
            super::InstallOptions {
                db_path,
                root,
                version: member.member_version.clone(),
                selection_reason: Some(&reason),
                sandbox_mode,
                ..Default::default()
            },
        )
        .await
        {
            Ok(_) => {
                installed_count += 1;
            }
            Err(e) => {
                eprintln!("  Failed to install {}: {}", member.member_name, e);
                failed_count += 1;
            }
        }
    }

    println!("\nCollection install complete:");
    println!("  Installed: {} package(s)", installed_count);
    println!("  Already installed: {} package(s)", skipped_count);
    if failed_count > 0 {
        println!("  Failed: {} package(s)", failed_count);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use conary_core::db::models::{CollectionMember, Trove, TroveType};

    /// `system init` equivalent: create an empty database with the current schema.
    fn setup_collection_test_db() -> (tempfile::TempDir, String) {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("conary.db").display().to_string();
        conary_core::db::init(&db_path).unwrap();
        (temp_dir, db_path)
    }

    fn collection_trove(db_path: &str, name: &str) -> Trove {
        let conn = conary_core::db::open(db_path).unwrap();
        Trove::find_by_name(&conn, name)
            .unwrap()
            .into_iter()
            .find(|trove| trove.trove_type == TroveType::Collection)
            .expect("collection trove must be persisted")
    }

    #[test]
    fn collection_create_list_show_reads_back_typed_record() {
        let (_temp_dir, db_path) = setup_collection_test_db();
        let members = vec!["gcc".to_string(), "make".to_string()];

        cmd_collection_create("dev-tools", Some("Development tools"), &members, &db_path)
            .expect("collection create must accept the placeholder version");

        let collection = collection_trove(&db_path, "dev-tools");
        assert_eq!(collection.trove_type, TroveType::Collection);
        assert_eq!(collection.name, "dev-tools");
        assert_eq!(collection.version, COLLECTION_TROVE_VERSION);
        assert_eq!(collection.description.as_deref(), Some("Development tools"));

        let collection_id = collection.id.expect("persisted collection must have an id");
        let conn = conary_core::db::open(&db_path).unwrap();
        let stored_members = CollectionMember::find_by_collection(&conn, collection_id).unwrap();
        let member_names: Vec<&str> = stored_members
            .iter()
            .map(|member| member.member_name.as_str())
            .collect();
        assert_eq!(member_names, vec!["gcc", "make"]);

        cmd_collection_list(&db_path).expect("collection list must succeed");
        cmd_collection_show("dev-tools", &db_path).expect("collection show must succeed");
    }

    #[test]
    fn collection_create_refuses_duplicate_name() {
        let (_temp_dir, db_path) = setup_collection_test_db();
        let members = vec!["gcc".to_string()];

        // Positive control: the same fixture and inputs must succeed once, so the
        // refusal below cannot come from a broken database or fixture.
        cmd_collection_create("dev-tools", None, &members, &db_path)
            .expect("positive control: first collection create must succeed");

        let duplicate = cmd_collection_create("dev-tools", None, &members, &db_path);
        assert!(
            duplicate.is_err(),
            "creating a collection with an existing name must be refused"
        );

        // The refusal must leave exactly the one collection the control created.
        let conn = conary_core::db::open(&db_path).unwrap();
        let collections = Trove::find_by_name(&conn, "dev-tools")
            .unwrap()
            .into_iter()
            .filter(|trove| trove.trove_type == TroveType::Collection)
            .count();
        assert_eq!(collections, 1);
    }
}
