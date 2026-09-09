// apps/conary/src/commands/install/batch/tests/preview.rs

use super::*;
use crate::commands::install::preview::PreviewDatabase;
use conary_core::db::models::{Repository, RepositoryPackage};
use conary_core::repository::enrollment::{LastOwnerDisposition, transaction as enrollment};
use conary_core::repository::versioning::VersionScheme;

fn seed_owner(conn: &mut rusqlite::Connection, package: &PreparedPackage) -> Trove {
    let tx = conn.transaction().unwrap();
    let changeset = Changeset::new("Repository owner fixture".into())
        .insert(&tx)
        .unwrap();
    let mut trove = package.to_trove(changeset).unwrap();
    let id = trove.insert(&tx).unwrap();
    enrollment::apply_transition(
        &tx,
        None,
        id,
        &package.name,
        &package.version,
        &package.repository_enrollments,
    )
    .unwrap();
    tx.commit().unwrap();
    trove
}

fn cache_candidate(conn: &rusqlite::Connection) -> i64 {
    let id = Repository::find_by_name(conn, "browser")
        .unwrap()
        .unwrap()
        .id
        .unwrap();
    RepositoryPackage::new(
        id,
        "later-update".into(),
        "2".into(),
        VersionScheme::Rpm,
        "fixture".into(),
        1,
        "https://repo.example/later.rpm".into(),
    )
    .insert(conn)
    .unwrap()
}

#[test]
fn preview_projects_incoming_and_replaced_repository_authority() {
    let (_temp, db_path) = crate::commands::test_helpers::create_test_db();
    let mut conn = conary_core::db::open(&db_path).unwrap();
    let key = repository_certificate();
    let old = prepared_repository_package(
        "repository-release",
        "1",
        "https://repo.example/old/$basearch",
        &key,
    );
    let trove = seed_owner(&mut conn, &old);
    let candidate = cache_candidate(&conn);
    let before = crate::commands::test_helpers::database_rows(&conn);
    let projection = PreviewDatabase::new(&conn, &db_path).unwrap();
    let mut new = prepared_repository_package(
        "repository-release",
        "2",
        "https://repo.example/new/$basearch",
        &key,
    );
    new.is_upgrade = true;
    new.old_trove = Some(Box::new(trove));
    projection.project(&[new]).unwrap();
    let projected = conary_core::db::open(projection.path()).unwrap();
    assert_eq!(
        Repository::find_by_name(&projected, "browser")
            .unwrap()
            .unwrap()
            .url,
        "https://repo.example/new/x86_64"
    );
    assert!(
        RepositoryPackage::find_by_id(&projected, candidate)
            .unwrap()
            .is_none()
    );
    assert_eq!(crate::commands::test_helpers::database_rows(&conn), before);
}

#[test]
fn preview_releases_dropped_repository_enrollments_before_later_selection() {
    let (_temp, db_path) = crate::commands::test_helpers::create_test_db();
    let mut conn = conary_core::db::open(&db_path).unwrap();
    let key = repository_certificate();
    let old = prepared_repository_package(
        "repository-release",
        "1",
        "https://repo.example/old/$basearch",
        &key,
    );
    let trove = seed_owner(&mut conn, &old);
    let candidate = cache_candidate(&conn);
    let before = crate::commands::test_helpers::database_rows(&conn);
    let projection = PreviewDatabase::new(&conn, &db_path).unwrap();
    let mut new = prepared_test_package("repository-release", "/usr/bin/new", b"new");
    new.version = "2".into();
    new.provides = vec![crate::commands::test_helpers::exact_package_self_provider(
        &new.name,
        &new.version,
        VersionScheme::Rpm,
    )];
    new.is_upgrade = true;
    new.old_trove = Some(Box::new(trove));
    projection.project(&[new]).unwrap();
    let projected = conary_core::db::open(projection.path()).unwrap();
    assert!(
        Repository::find_by_name(&projected, "browser")
            .unwrap()
            .is_none()
    );
    assert!(
        RepositoryPackage::find_by_id(&projected, candidate)
            .unwrap()
            .is_none()
    );
    assert_eq!(crate::commands::test_helpers::database_rows(&conn), before);
}

#[test]
fn preview_keeps_shared_repository_candidates_across_an_owner_batch() {
    let (_temp, db_path) = crate::commands::test_helpers::create_test_db();
    let mut conn = conary_core::db::open(&db_path).unwrap();
    let key = repository_certificate();
    let mut packages = Vec::new();
    for name in ["repository-owner-a", "repository-owner-b"] {
        let old =
            prepared_repository_package(name, "1", "https://repo.example/shared/$basearch", &key);
        let trove = seed_owner(&mut conn, &old);
        let mut new =
            prepared_repository_package(name, "2", "https://repo.example/shared/$basearch", &key);
        new.is_upgrade = true;
        new.old_trove = Some(Box::new(trove));
        packages.push(new);
    }
    let candidate = cache_candidate(&conn);
    let before = crate::commands::test_helpers::database_rows(&conn);
    let projection = PreviewDatabase::new(&conn, &db_path).unwrap();
    projection.project(&packages).unwrap();
    let projected = conary_core::db::open(projection.path()).unwrap();
    assert!(
        RepositoryPackage::find_by_id(&projected, candidate)
            .unwrap()
            .is_some()
    );
    let owners: i64 = projected
        .query_row(
            "SELECT count(*) FROM package_repository_enrollments WHERE owner_kind = 'package'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(owners, 2);
    assert_eq!(crate::commands::test_helpers::database_rows(&conn), before);
}

#[test]
fn preview_honors_relation_removed_repository_last_owner_disposition() {
    use conary_core::repository::dependency_model::{
        PackageRelationRemovalMode, RepositoryRequirementKind,
    };
    use conary_core::transaction::{PackageRelationIncomingIdentity, PackageRelationRemoval};
    for disposition in [
        LastOwnerDisposition::RemoveWhenUnowned,
        LastOwnerDisposition::Retain,
    ] {
        let (_temp, db_path) = crate::commands::test_helpers::create_test_db();
        let mut conn = conary_core::db::open(&db_path).unwrap();
        let key = repository_certificate();
        let mut old = prepared_repository_package(
            "repository-release",
            "1",
            "https://repo.example/old/$basearch",
            &key,
        );
        old.repository_enrollments[0].last_owner = disposition;
        let trove = seed_owner(&mut conn, &old);
        let candidate = cache_candidate(&conn);
        let before = crate::commands::test_helpers::database_rows(&conn);
        let projection = PreviewDatabase::new(&conn, &db_path).unwrap();
        let mut new = prepared_test_package("replacement", "/usr/bin/new", b"new");
        new.relation_removals.push(PackageRelationRemoval {
            trove_id: trove.id.unwrap(),
            package_name: trove.name,
            package_version: trove.version,
            package_architecture: trove.architecture,
            triggering_incoming: PackageRelationIncomingIdentity {
                transaction_index: 0,
                package_name: new.name.clone(),
                package_version: new.version.clone(),
                package_architecture: new.architecture.clone(),
            },
            incoming_packages: vec![new.name.clone()],
            ownership_transfer_packages: vec![new.name.clone()],
            kind: RepositoryRequirementKind::Obsolete,
            mode: PackageRelationRemovalMode::OwnershipTransfer,
            native_text: Some("repository-release < 2".into()),
        });
        projection.project(&[new]).unwrap();
        let projected = conary_core::db::open(projection.path()).unwrap();
        let retained = disposition == LastOwnerDisposition::Retain;
        assert_eq!(
            RepositoryPackage::find_by_id(&projected, candidate)
                .unwrap()
                .is_some(),
            retained
        );
        let owners: i64 = projected
            .query_row(
                "SELECT count(*) FROM package_repository_enrollments WHERE owner_kind = 'retained'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(owners, i64::from(retained));
        assert_eq!(crate::commands::test_helpers::database_rows(&conn), before);
    }
}
