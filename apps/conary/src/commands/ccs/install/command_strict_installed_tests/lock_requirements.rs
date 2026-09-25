// apps/conary/src/commands/ccs/install/command_strict_installed_tests/lock_requirements.rs

use super::*;

/// The consumer's pre-dependency is satisfied by an installed file provider at
/// solve time, but another transaction removes that provider before the locked
/// transaction. The install must refuse with the typed requirements-change
/// error instead of committing with its provider gone. The positive control is
/// `consumer_predepends_installs_while_installed_provider_survives`.
#[tokio::test]
async fn ccs_install_refuses_when_the_required_provider_disappears_before_the_lock() {
    use conary_core::db::models::Trove;

    let _mount_guard = crate::commands::composefs_ops::test_mount_skip_guard();
    let temp_dir = tempfile::tempdir().unwrap();
    let install_root = temp_dir.path().join("root");
    let db_path = temp_dir.path().join("conary.db");
    let db_path_str = db_path.to_str().unwrap();
    let install_root_str = install_root.to_str().unwrap();

    std::fs::create_dir_all(&install_root).unwrap();
    conary_core::db::init(db_path_str).unwrap();
    stage_test_boot_assets(temp_dir.path());

    install_strict_provider(temp_dir.path(), db_path_str, install_root_str);
    let conn = conary_core::db::open(db_path_str).unwrap();
    let provider_id = Trove::find_by_name(&conn, "strict-provider")
        .unwrap()
        .remove(0)
        .id
        .unwrap();
    drop(conn);
    let consumer = write_strict_consumer(temp_dir.path(), false);

    // The seam runs with the mutation lock held and removes the provider the
    // command's dependency solve certified, so the locked re-solve cannot place
    // the consumer's pre-dependency.
    let hook_db_path = db_path_str.to_string();
    crate::commands::install::dependencies::set_after_mutation_lock_hook(move || {
        let conn = conary_core::db::open(&hook_db_path).unwrap();
        assert_eq!(
            conn.execute("DELETE FROM troves WHERE id = ?1", [provider_id])
                .unwrap(),
            1,
            "the seam must remove the required provider"
        );
    });

    let error = run_install(&consumer, db_path_str, install_root_str, false)
        .expect_err("a CCS install accepted a required provider removed before it locked");
    crate::commands::install::dependencies::clear_after_mutation_lock_hook();
    let changed = error
        .downcast_ref::<crate::commands::install::dependencies::RequirementsChanged>()
        .expect("refusal must carry the typed requirements-change error");
    assert_eq!(changed.package, "strict-consumer");
    assert!(
        changed.conflict.is_some() || !changed.missing.is_empty(),
        "{changed:?}"
    );

    let conn = conary_core::db::open(db_path_str).unwrap();
    assert!(
        conary_core::db::models::Trove::find_by_name(&conn, "strict-consumer")
            .unwrap()
            .is_empty(),
        "a refused consumer must not be persisted"
    );
}
