// apps/conary/src/commands/install/command_mutation_lock_tests.rs

use super::*;
use conary_core::db::models::{InstallSource, Trove, TroveType};
use conary_core::packages::PackageFormat;
use conary_core::repository::versioning::VersionScheme;

#[test]
fn native_install_resolves_upgrade_identity_under_the_mutation_lock() {
    let _mount_skip = crate::commands::composefs_ops::test_mount_skip_guard();
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("conary.db");
    let install_root = temp.path().join("install-root");
    std::fs::create_dir_all(&install_root).unwrap();
    conary_core::db::init(&db_path).unwrap();
    let (fixture_user, fixture_group) =
        crate::commands::test_helpers::seed_unprivileged_fixture_owner(&db_path);

    let mut builder = rpm::PackageBuilder::new(
        "native-lock-fixture",
        "2.0.0",
        "MIT",
        "x86_64",
        "native install mutation-lock fixture",
    );
    builder
        .with_file_contents(
            b"fixture\n".to_vec(),
            rpm::FileOptions::new("/usr/lib/native-lock-fixture/payload")
                .permissions(0o644)
                .user(fixture_user)
                .group(fixture_group),
        )
        .unwrap();
    let rpm_path = temp.path().join("native-lock-fixture.rpm");
    builder.build().unwrap().write_file(&rpm_path).unwrap();
    let incoming =
        conary_core::packages::rpm::RpmPackage::parse(rpm_path.to_str().unwrap()).unwrap();
    let incoming_version = incoming.version().to_string();

    let conn = conary_core::db::open(&db_path).unwrap();
    let mut installed = Trove::new_with_source(
        "native-lock-fixture".to_string(),
        "1.0.0-1".to_string(),
        TroveType::Package,
        InstallSource::Repository,
        VersionScheme::Rpm,
    );
    installed.architecture = Some("x86_64".to_string());
    let installed_id = installed.insert(&conn).unwrap();
    drop(conn);

    let db_path_string = db_path.to_string_lossy().into_owned();
    let install_root_string = install_root.to_string_lossy().into_owned();
    let rpm_path_string = rpm_path.to_string_lossy().into_owned();
    let locked = LockedRuntimeRoot::acquire(&db_path_string).unwrap();
    let (attempt_tx, attempt_rx) = std::sync::mpsc::sync_channel(0);
    let (result_tx, result_rx) = std::sync::mpsc::sync_channel(0);
    let waiter_db_path = db_path_string.clone();
    let waiter = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        attempt_tx.send(()).unwrap();
        let result = runtime
            .block_on(cmd_install(
                &rpm_path_string,
                InstallOptions {
                    db_path: &waiter_db_path,
                    root: &install_root_string,
                    architecture: Some("x86_64".to_string()),
                    no_deps: true,
                    sandbox_mode: crate::commands::SandboxMode::Always,
                    yes: true,
                    ..InstallOptions::default()
                },
            ))
            .map_err(|error| format!("{error:#}"));
        result_tx.send(result).unwrap();
    });

    attempt_rx.recv().unwrap();
    assert!(
        matches!(
            result_rx.recv_timeout(std::time::Duration::from_secs(1)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ),
        "native install reached a verdict while another transaction held the mutation lock"
    );

    let conn = conary_core::db::open(&db_path).unwrap();
    assert_eq!(
        conn.execute(
            "UPDATE troves SET version = ?1 WHERE id = ?2",
            rusqlite::params![incoming_version, installed_id],
        )
        .unwrap(),
        1
    );
    drop(conn);
    drop(locked);

    let error = result_rx
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("native install must finish once the mutation lock is released")
        .expect_err("native install accepted upgrade authority changed before it locked");
    waiter.join().unwrap();
    assert!(error.contains("is already installed"), "{error}");
}

struct NativeUpgradeFixture {
    db_path_string: String,
    install_root: String,
    rpm_path: String,
    installed_id: i64,
}

/// Build the incoming RPM and install the older trove it upgrades. The solve's
/// outgoing projection is exactly that replacement target.
fn native_upgrade_fixture(temp: &std::path::Path) -> NativeUpgradeFixture {
    let db_path = temp.join("conary.db");
    let install_root = temp.join("install-root");
    std::fs::create_dir_all(&install_root).unwrap();
    conary_core::db::init(&db_path).unwrap();
    let (fixture_user, fixture_group) =
        crate::commands::test_helpers::seed_unprivileged_fixture_owner(&db_path);

    let mut builder = rpm::PackageBuilder::new(
        "native-lock-fixture",
        "2.0.0",
        "MIT",
        "x86_64",
        "native install mutation-lock fixture",
    );
    builder
        .with_file_contents(
            b"fixture\n".to_vec(),
            rpm::FileOptions::new("/usr/lib/native-lock-fixture/payload")
                .permissions(0o644)
                .user(fixture_user)
                .group(fixture_group),
        )
        .unwrap();
    let rpm_path = temp.join("native-lock-fixture.rpm");
    builder.build().unwrap().write_file(&rpm_path).unwrap();

    let conn = conary_core::db::open(&db_path).unwrap();
    let mut installed = Trove::new_with_source(
        "native-lock-fixture".to_string(),
        "1.0.0-1".to_string(),
        TroveType::Package,
        InstallSource::Repository,
        VersionScheme::Rpm,
    );
    installed.architecture = Some("x86_64".to_string());
    let installed_id = installed.insert(&conn).unwrap();
    drop(conn);

    NativeUpgradeFixture {
        db_path_string: db_path.to_string_lossy().into_owned(),
        install_root: install_root.to_string_lossy().into_owned(),
        rpm_path: rpm_path.to_string_lossy().into_owned(),
        installed_id,
    }
}

/// One native install run to a verdict on this thread. Running the sink here,
/// rather than racing a separate thread against the mutation lock, lets the
/// post-lock seam fire deterministically.
fn run_native_install(fixture: &NativeUpgradeFixture) -> Result<(), anyhow::Error> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(cmd_install(
        &fixture.rpm_path,
        InstallOptions {
            db_path: &fixture.db_path_string,
            root: &fixture.install_root,
            architecture: Some("x86_64".to_string()),
            no_deps: true,
            sandbox_mode: crate::commands::SandboxMode::Always,
            yes: true,
            ..InstallOptions::default()
        },
    ))
}

#[test]
fn native_install_refuses_an_outgoing_set_changed_before_the_mutation_lock() {
    let _mount_skip = crate::commands::composefs_ops::test_mount_skip_guard();
    let temp = tempfile::tempdir().unwrap();
    let fixture = native_upgrade_fixture(temp.path());

    // The seam runs with the mutation lock held and removes the replacement
    // target the solve projected, so the locked transaction resolves an empty
    // outgoing set.
    let hook_db_path = fixture.db_path_string.clone();
    let installed_id = fixture.installed_id;
    super::super::dependencies::set_after_mutation_lock_hook(move || {
        let conn = conary_core::db::open(&hook_db_path).unwrap();
        assert_eq!(
            conn.execute("DELETE FROM troves WHERE id = ?1", [installed_id])
                .unwrap(),
            1,
            "the seam must remove the projected replacement target"
        );
    });

    let error = run_native_install(&fixture)
        .expect_err("native install accepted an outgoing set changed before it locked");
    super::super::dependencies::clear_after_mutation_lock_hook();
    let changed = error
        .downcast_ref::<super::super::dependencies::OutgoingSetChanged>()
        .expect("refusal must carry the typed outgoing-set error");
    assert_eq!(changed.projected, vec![installed_id]);
    assert!(changed.locked.is_empty(), "{changed:?}");
}

#[test]
fn native_install_proceeds_when_the_certified_outgoing_set_is_unchanged() {
    let _mount_skip = crate::commands::composefs_ops::test_mount_skip_guard();
    let temp = tempfile::tempdir().unwrap();
    let fixture = native_upgrade_fixture(temp.path());

    // Positive control: the same fixture without an armed seam resolves the
    // same replacement target, so certification must not refuse.
    let verdict = run_native_install(&fixture);
    super::super::dependencies::clear_after_mutation_lock_hook();
    if let Err(error) = verdict {
        assert!(
            error
                .downcast_ref::<super::super::dependencies::OutgoingSetChanged>()
                .is_none(),
            "an unchanged outgoing set must not be refused: {error:#}"
        );
    }
}

struct NativeObsoleteFixture {
    db_path_string: String,
    install_root: String,
    rpm_path: String,
    obsolete_id: i64,
}

/// Install the trove the incoming package obsoletes, then build the incoming
/// RPM. The transaction's only outgoing trove is that relation removal.
fn native_obsolete_fixture(temp: &std::path::Path) -> NativeObsoleteFixture {
    let db_path = temp.join("conary.db");
    let install_root = temp.join("install-root");
    std::fs::create_dir_all(&install_root).unwrap();
    conary_core::db::init(&db_path).unwrap();
    let (fixture_user, fixture_group) =
        crate::commands::test_helpers::seed_unprivileged_fixture_owner(&db_path);
    let db_path_string = db_path.to_string_lossy().into_owned();
    let install_root_string = install_root.to_string_lossy().into_owned();

    let mut target_builder = rpm::PackageBuilder::new(
        "native-obsolete-target",
        "1.0.0",
        "MIT",
        "x86_64",
        "native install obsolete target",
    );
    target_builder
        .with_file_contents(
            b"target\n".to_vec(),
            rpm::FileOptions::new("/usr/lib/native-obsolete-target/payload")
                .permissions(0o644)
                .user(fixture_user)
                .group(fixture_group),
        )
        .unwrap();
    let target_path = temp.join("native-obsolete-target.rpm");
    target_builder
        .build()
        .unwrap()
        .write_file(&target_path)
        .unwrap();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime
        .block_on(cmd_install(
            target_path.to_str().unwrap(),
            InstallOptions {
                db_path: &db_path_string,
                root: &install_root_string,
                architecture: Some("x86_64".to_string()),
                no_deps: true,
                sandbox_mode: crate::commands::SandboxMode::Always,
                yes: true,
                ..InstallOptions::default()
            },
        ))
        .expect("the obsolete target fixture must install");

    let obsolete_id = {
        let conn = conary_core::db::open(&db_path).unwrap();
        Trove::find_by_name(&conn, "native-obsolete-target")
            .unwrap()
            .remove(0)
            .id
            .unwrap()
    };

    let mut builder = rpm::PackageBuilder::new(
        "native-obsolete-incoming",
        "1.0.0",
        "MIT",
        "x86_64",
        "native install obsolete mutation-lock fixture",
    );
    builder.obsoletes(rpm::Dependency::any("native-obsolete-target"));
    builder
        .with_file_contents(
            b"fixture\n".to_vec(),
            rpm::FileOptions::new("/usr/lib/native-obsolete-incoming/payload")
                .permissions(0o644)
                .user(fixture_user)
                .group(fixture_group),
        )
        .unwrap();
    let rpm_path = temp.join("native-obsolete-incoming.rpm");
    builder.build().unwrap().write_file(&rpm_path).unwrap();

    NativeObsoleteFixture {
        db_path_string,
        install_root: install_root_string,
        rpm_path: rpm_path.to_string_lossy().into_owned(),
        obsolete_id,
    }
}

/// An install whose only outgoing trove is a relation removal must project and
/// resolve that removal identically, so certification proceeds.
#[test]
fn native_install_proceeds_when_it_obsoletes_an_installed_trove() {
    let _mount_skip = crate::commands::composefs_ops::test_mount_skip_guard();
    let temp = tempfile::tempdir().unwrap();
    let fixture = native_obsolete_fixture(temp.path());

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime
        .block_on(cmd_install(
            &fixture.rpm_path,
            InstallOptions {
                db_path: &fixture.db_path_string,
                root: &fixture.install_root,
                architecture: Some("x86_64".to_string()),
                no_deps: true,
                sandbox_mode: crate::commands::SandboxMode::Always,
                yes: true,
                ..InstallOptions::default()
            },
        ))
        .expect("an install that obsoletes an installed trove must proceed");

    let conn = conary_core::db::open(&fixture.db_path_string).unwrap();
    assert!(
        Trove::find_by_id(&conn, fixture.obsolete_id)
            .unwrap()
            .is_none(),
        "the obsoleted trove must be removed"
    );
    assert_eq!(
        Trove::find_by_name(&conn, "native-obsolete-incoming")
            .unwrap()
            .len(),
        1,
    );
}

struct NativeRequirementFixture {
    db_path_string: String,
    install_root: String,
    rpm_path: String,
    provider_id: i64,
}

/// Install the provider the incoming package hard-requires, then build the
/// incoming RPM. The pre-lock dependency solve places the requirement against
/// that installed provider.
fn native_requirement_fixture(temp: &std::path::Path) -> NativeRequirementFixture {
    let db_path = temp.join("conary.db");
    let install_root = temp.join("install-root");
    std::fs::create_dir_all(&install_root).unwrap();
    conary_core::db::init(&db_path).unwrap();
    let (fixture_user, fixture_group) =
        crate::commands::test_helpers::seed_unprivileged_fixture_owner(&db_path);
    let db_path_string = db_path.to_string_lossy().into_owned();
    let install_root_string = install_root.to_string_lossy().into_owned();

    let mut provider_builder = rpm::PackageBuilder::new(
        "native-required-provider",
        "1.0.0",
        "MIT",
        "x86_64",
        "native install required provider",
    );
    provider_builder
        .with_file_contents(
            b"provider\n".to_vec(),
            rpm::FileOptions::new("/usr/lib/native-required-provider/payload")
                .permissions(0o644)
                .user(fixture_user)
                .group(fixture_group),
        )
        .unwrap();
    let provider_path = temp.join("native-required-provider.rpm");
    provider_builder
        .build()
        .unwrap()
        .write_file(&provider_path)
        .unwrap();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime
        .block_on(cmd_install(
            provider_path.to_str().unwrap(),
            InstallOptions {
                db_path: &db_path_string,
                root: &install_root_string,
                architecture: Some("x86_64".to_string()),
                no_deps: true,
                sandbox_mode: crate::commands::SandboxMode::Always,
                yes: true,
                ..InstallOptions::default()
            },
        ))
        .expect("the required provider fixture must install");

    let provider_id = {
        let conn = conary_core::db::open(&db_path).unwrap();
        Trove::find_by_name(&conn, "native-required-provider")
            .unwrap()
            .remove(0)
            .id
            .unwrap()
    };

    let mut builder = rpm::PackageBuilder::new(
        "native-required-consumer",
        "1.0.0",
        "MIT",
        "x86_64",
        "native install requirement mutation-lock fixture",
    );
    builder.requires(rpm::Dependency::any("native-required-provider"));
    builder
        .with_file_contents(
            b"consumer\n".to_vec(),
            rpm::FileOptions::new("/usr/lib/native-required-consumer/payload")
                .permissions(0o644)
                .user(fixture_user)
                .group(fixture_group),
        )
        .unwrap();
    let rpm_path = temp.join("native-required-consumer.rpm");
    builder.build().unwrap().write_file(&rpm_path).unwrap();

    NativeRequirementFixture {
        db_path_string,
        install_root: install_root_string,
        rpm_path: rpm_path.to_string_lossy().into_owned(),
        provider_id,
    }
}

fn run_native_requirement_install(fixture: &NativeRequirementFixture) -> Result<(), anyhow::Error> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(cmd_install(
        &fixture.rpm_path,
        InstallOptions {
            db_path: &fixture.db_path_string,
            root: &fixture.install_root,
            architecture: Some("x86_64".to_string()),
            sandbox_mode: crate::commands::SandboxMode::Always,
            yes: true,
            ..InstallOptions::default()
        },
    ))
}

/// Another transaction removes the provider that alone satisfied the incoming
/// package's hard requirement in the window after the pre-lock dependency solve
/// but before the mutation lock. The locked transaction must refuse rather than
/// commit with the provider gone.
#[test]
fn native_install_refuses_when_a_required_provider_disappears_before_the_mutation_lock() {
    let _mount_skip = crate::commands::composefs_ops::test_mount_skip_guard();
    let temp = tempfile::tempdir().unwrap();
    let fixture = native_requirement_fixture(temp.path());

    let hook_db_path = fixture.db_path_string.clone();
    let provider_id = fixture.provider_id;
    super::super::dependencies::set_after_mutation_lock_hook(move || {
        let conn = conary_core::db::open(&hook_db_path).unwrap();
        assert_eq!(
            conn.execute("DELETE FROM troves WHERE id = ?1", [provider_id])
                .unwrap(),
            1,
            "the seam must remove the required provider"
        );
    });

    let error = run_native_requirement_install(&fixture)
        .expect_err("native install accepted a required provider removed before it locked");
    super::super::dependencies::clear_after_mutation_lock_hook();
    let changed = error
        .downcast_ref::<super::super::dependencies::RequirementsChanged>()
        .expect("refusal must carry the typed requirements-change error");
    assert_eq!(changed.package, "native-required-consumer");
    assert!(
        changed.conflict.is_some() || !changed.missing.is_empty(),
        "{changed:?}"
    );

    let conn = conary_core::db::open(&fixture.db_path_string).unwrap();
    assert!(
        Trove::find_by_name(&conn, "native-required-consumer")
            .unwrap()
            .is_empty(),
        "a refused consumer must not be persisted"
    );
}

/// Positive control: the identical fixture with no armed seam resolves the same
/// installed provider under the lock and installs the consumer.
#[test]
fn native_install_proceeds_when_a_required_provider_survives() {
    let _mount_skip = crate::commands::composefs_ops::test_mount_skip_guard();
    let temp = tempfile::tempdir().unwrap();
    let fixture = native_requirement_fixture(temp.path());

    super::super::dependencies::clear_after_mutation_lock_hook();
    run_native_requirement_install(&fixture)
        .expect("an install whose required provider survives must proceed");

    let conn = conary_core::db::open(&fixture.db_path_string).unwrap();
    assert_eq!(
        Trove::find_by_name(&conn, "native-required-consumer")
            .unwrap()
            .len(),
        1,
        "the consumer must be persisted when its provider survives"
    );
}
