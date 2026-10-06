// apps/conary/src/commands/install/batch/tests/mutation_lock.rs

#![cfg(test)]

//! Certification reads installed state with the mutation lock already held.
//!
//! A batch is admitted on facts it reads from installed state: which
//! requirements the end-state universe satisfies, which promises it relies on,
//! which installed packages the incoming relations remove. Reading those facts
//! before waiting for the runtime mutation lock leaves a window -- another
//! transaction can delete a provider the certification leaned on, and this
//! batch still commits against a universe that no longer exists. The lock is
//! what closes that window, so the facts have to be read on its far side.

use super::*;

/// Delete the only provider while the batch waits for the mutation lock.
///
/// The deletion lands after the batch has started and before it holds the
/// lock, which is exactly the interval a pre-lock snapshot would have read
/// across. Certification must see the deletion, because the database it is
/// about to commit into is the one without the provider.
#[test]
fn a_batch_certifies_against_the_installed_state_the_mutation_lock_protects() {
    let _mount_guard = crate::commands::composefs_ops::test_mount_skip_guard();
    let temp = tempfile::tempdir().unwrap();
    let (db_path, db_path_string) = db_with_prior_transaction(
        temp.path(),
        vec![prepared_test_package(
            "provider",
            "/usr/lib64/libprovided.so.1",
            b"provided",
        )],
    );

    // Hold the mutation lock without materializing a root, which is the state
    // the installer itself is in while it certifies.
    let locked =
        crate::commands::generation::selected_root::LockedRuntimeRoot::acquire(&db_path_string)
            .unwrap();

    let (attempt_tx, attempt_rx) = std::sync::mpsc::sync_channel(0);
    let (result_tx, result_rx) = std::sync::mpsc::sync_channel(0);
    let waiter_db_path = db_path_string.clone();
    let waiter = std::thread::spawn(move || {
        let mut dependent = prepared_test_package("dependent", "/usr/bin/dependent", b"dependent");
        dependent.requirements = vec![depends_on("provider")];
        attempt_tx.send(()).unwrap();
        let result = BatchInstaller::new(&waiter_db_path, SandboxMode::Always)
            .install_batch(vec![dependent])
            .map_err(|error| format!("{error:#}"));
        result_tx.send(result).unwrap();
    });

    // No verdict is reachable while another holder owns the lock.
    attempt_rx.recv().unwrap();
    assert!(
        matches!(
            result_rx.recv_timeout(std::time::Duration::from_millis(250)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ),
        "a batch reached a verdict while another transaction held the mutation lock"
    );

    // Remove the only provider under the held lock. Nothing this batch will
    // certify has been read yet, so this is the state it is obliged to see.
    let conn = conary_core::db::open(&db_path).unwrap();
    let deleted = conn
        .execute("DELETE FROM troves WHERE name = 'provider'", [])
        .unwrap();
    assert_eq!(
        deleted, 1,
        "the prior transaction must have installed exactly one provider"
    );
    drop(conn);
    drop(locked);

    let error = result_rx
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("the batch must reach a verdict once the mutation lock is released")
        .expect_err("a batch certified a requirement against a provider deleted before it locked");
    waiter.join().unwrap();
    assert!(
        error.contains("does not satisfy exact depends requirement for dependent"),
        "{error}"
    );
}

fn obsoletes(name: &str) -> conary_core::repository::dependency_model::RepositoryRequirementGroup {
    use conary_core::repository::dependency_model::{
        RepositoryRequirementClause, RepositoryRequirementGroup, RepositoryRequirementKind,
    };

    RepositoryRequirementGroup::simple(
        RepositoryRequirementKind::Obsolete,
        RepositoryRequirementClause::name_only(name.to_string()),
    )
}

/// An installed trove the incoming relation removes is part of the batch's
/// outgoing set. The seam runs with the mutation lock held and inserts a second
/// match the pre-lock projection never saw, so the locked set differs.
#[test]
fn a_batch_refuses_an_outgoing_set_changed_before_the_mutation_lock() {
    let _mount_guard = crate::commands::composefs_ops::test_mount_skip_guard();
    let temp = tempfile::tempdir().unwrap();
    let (db_path, db_path_string) = db_with_prior_transaction(
        temp.path(),
        vec![prepared_test_package(
            "legacy-tool",
            "/usr/bin/legacy-tool",
            b"legacy",
        )],
    );
    let conn = conary_core::db::open(&db_path).unwrap();
    let legacy_id = Trove::find_by_name(&conn, "legacy-tool")
        .unwrap()
        .remove(0)
        .id
        .unwrap();
    drop(conn);

    let mut incoming = prepared_test_package("modern-tool", "/usr/bin/modern-tool", b"modern");
    incoming.relations = vec![obsoletes("legacy-tool")];
    let conn = conary_core::db::open(&db_path).unwrap();
    let certified = crate::commands::install::batch::project_batch_outgoing(
        &conn,
        std::slice::from_ref(&incoming),
    )
    .unwrap();
    drop(conn);

    let hook_db_path = db_path_string.clone();
    crate::commands::install::dependencies::set_after_mutation_lock_hook(move || {
        let conn = conary_core::db::open(&hook_db_path).unwrap();
        let mut extra = Trove::new(
            "legacy-tool".to_string(),
            "2.0.0".to_string(),
            TroveType::Package,
            conary_core::repository::versioning::VersionScheme::Rpm,
        );
        extra.architecture = Some("x86_64".to_string());
        extra
            .insert(&conn)
            .expect("the seam must install a second relation-removal target");
    });

    let error = BatchInstaller::new(&db_path_string, SandboxMode::Always)
        .with_certified_outgoing(Some(certified))
        .install_batch(vec![incoming])
        .expect_err("a batch accepted an outgoing set changed before it locked");
    crate::commands::install::dependencies::clear_after_mutation_lock_hook();
    let changed = error
        .downcast_ref::<crate::commands::install::dependencies::OutgoingSetChanged>()
        .expect("refusal must carry the typed outgoing-set error");
    assert_eq!(changed.projected, vec![legacy_id]);
    assert_eq!(changed.locked.len(), 2, "{changed:?}");
    assert!(changed.locked.contains(&legacy_id), "{changed:?}");
}

/// Positive control: the identical batch with no armed seam resolves exactly
/// the certified outgoing set and installs.
#[test]
fn a_batch_proceeds_when_the_certified_outgoing_set_is_unchanged() {
    let _mount_guard = crate::commands::composefs_ops::test_mount_skip_guard();
    let temp = tempfile::tempdir().unwrap();
    let (db_path, db_path_string) = db_with_prior_transaction(
        temp.path(),
        vec![prepared_test_package(
            "legacy-tool",
            "/usr/bin/legacy-tool",
            b"legacy",
        )],
    );
    let mut incoming = prepared_test_package("modern-tool", "/usr/bin/modern-tool", b"modern");
    incoming.relations = vec![obsoletes("legacy-tool")];
    let conn = conary_core::db::open(&db_path).unwrap();
    let certified = crate::commands::install::batch::project_batch_outgoing(
        &conn,
        std::slice::from_ref(&incoming),
    )
    .unwrap();
    drop(conn);

    crate::commands::install::dependencies::clear_after_mutation_lock_hook();
    BatchInstaller::new(&db_path_string, SandboxMode::Always)
        .with_certified_outgoing(Some(certified))
        .install_batch(vec![incoming])
        .expect("an unchanged certified outgoing set must install");

    let conn = conary_core::db::open(&db_path).unwrap();
    assert!(
        Trove::find_by_name(&conn, "legacy-tool")
            .unwrap()
            .is_empty(),
        "the outgoing relation removal must commit"
    );
}

fn generic_provide(name: &str) -> conary_core::resolver::identity::ProvidedCapability {
    conary_core::resolver::identity::ProvidedCapability {
        kind: conary_core::repository::dependency_model::RepositoryCapabilityKind::Generic,
        name: name.to_string(),
        version: None,
        version_relation: None,
        version_scheme: conary_core::repository::versioning::VersionScheme::Rpm,
        architecture_qualifier:
            conary_core::repository::dependency_model::ProvideArchitectureQualifier::Implicit,
        provenance: conary_core::repository::dependency_model::CapabilityProvenance::AuthorDeclared,
    }
}

/// Insert one installed RPM-versioned trove whose only hard group is
/// `(foo if bar)`.
fn insert_installed_conditional_trove(db_path: &str, name: &str) -> i64 {
    use conary_core::repository::dependency_model::RepositoryRequirementKind;

    let conn = conary_core::db::open(db_path).unwrap();
    let mut installed = Trove::new(
        name.to_string(),
        "1.0.0".to_string(),
        TroveType::Package,
        conary_core::repository::versioning::VersionScheme::Rpm,
    );
    installed.architecture = Some("x86_64".to_string());
    let trove_id = installed.insert(&conn).unwrap();
    let requirement = conary_core::repository::requirement::parse_native_requirement(
        RepositoryRequirementKind::Depends,
        conary_core::repository::versioning::VersionScheme::Rpm,
        "(foo if bar)",
    )
    .unwrap();
    conary_core::db::models::InstalledRequirementGroup::insert_groups(
        &conn,
        trove_id,
        conary_core::repository::versioning::VersionScheme::Rpm,
        &[requirement],
    )
    .unwrap();
    trove_id
}

fn certified_end_state_batch() -> Vec<PreparedPackage> {
    let mut dependency = prepared_test_package("dep-lib", "/usr/lib64/libdep-lib.so.1", b"dep");
    dependency.install_reason = InstallReason::Dependency;
    let mut root = prepared_test_package("root-tool", "/usr/bin/root-tool", b"root");
    root.provides.push(generic_provide("bar"));
    root.requirements = vec![depends_on("dep-lib")];
    vec![dependency, root]
}

fn seeded_db(temp: &std::path::Path) -> (std::path::PathBuf, String) {
    let db_path = temp.join("conary.db");
    std::fs::create_dir_all(temp.join("root")).unwrap();
    conary_core::db::init(&db_path).unwrap();
    crate::commands::test_helpers::seed_test_bootable_runtime(&db_path);
    let db_path_string = db_path.to_string_lossy().into_owned();
    (db_path, db_path_string)
}

/// The caller solved the batch against installed state before the mutation
/// lock. Another transaction installs `x`, whose `(foo if bar)` the incoming
/// root's `bar` activates, after the batch acquires the lock. The locked batch
/// must re-certify the complete end state and refuse.
#[test]
fn a_batch_refuses_an_end_state_broken_after_the_dependency_solve() {
    let _mount_guard = crate::commands::composefs_ops::test_mount_skip_guard();
    let temp = tempfile::tempdir().unwrap();
    let (_db_path, db_path_string) = seeded_db(temp.path());

    let hook_db_path = db_path_string.clone();
    crate::commands::install::dependencies::set_after_mutation_lock_hook(move || {
        insert_installed_conditional_trove(&hook_db_path, "x");
    });

    let error = BatchInstaller::new(&db_path_string, SandboxMode::Always)
        .with_certified_end_state()
        .install_batch(certified_end_state_batch())
        .expect_err("a batch accepted an end state broken before it locked");
    crate::commands::install::dependencies::clear_after_mutation_lock_hook();

    let changed = error
        .downcast_ref::<crate::commands::install::dependencies::RequirementsChanged>()
        .expect("refusal must carry the typed requirements-change error");
    assert_eq!(changed.package, "root-tool");
    assert!(
        changed.unsatisfied.iter().any(|group| matches!(
            &group.owner,
            conary_core::resolver::sat::SatGroupOwner::Installed { package_name, .. }
                if package_name == "x"
        )),
        "the refusal must name the installed trove the incoming batch breaks: {changed:?}"
    );
}

/// Positive control: the identical batch with no armed seam certifies against a
/// fixed end state that holds and installs.
#[test]
fn a_batch_proceeds_when_the_fixed_end_state_holds() {
    let _mount_guard = crate::commands::composefs_ops::test_mount_skip_guard();
    let temp = tempfile::tempdir().unwrap();
    let (db_path, db_path_string) = seeded_db(temp.path());

    crate::commands::install::dependencies::clear_after_mutation_lock_hook();
    BatchInstaller::new(&db_path_string, SandboxMode::Always)
        .with_certified_end_state()
        .install_batch(certified_end_state_batch())
        .expect("a fixed end state that holds must install");

    let conn = conary_core::db::open(&db_path).unwrap();
    assert_eq!(
        Trove::find_by_name(&conn, "root-tool").unwrap().len(),
        1,
        "the root must be persisted"
    );
}
