// apps/conary/src/commands/install/command_mutation_lock_tests/conditional.rs

use super::*;

/// The capability the installed conditional tests on. The dependency-free
/// incoming package provides it to make the implication true.
const CONDITIONAL_TRIGGER: &str = "native-conditional-trigger";

/// Seed one installed RPM-versioned trove whose only hard group is
/// `(native-conditional-require if native-conditional-trigger)`.
fn insert_installed_conditional(db_path: &str, name: &str) -> i64 {
    use conary_core::repository::dependency_model::RepositoryRequirementKind;

    let conn = conary_core::db::open(db_path).unwrap();
    let mut installed = Trove::new_with_source(
        name.to_string(),
        "1.0.0".to_string(),
        TroveType::Package,
        InstallSource::Repository,
        VersionScheme::Rpm,
    );
    installed.architecture = Some("x86_64".to_string());
    let trove_id = installed.insert(&conn).unwrap();
    let requirement = conary_core::repository::requirement::parse_native_requirement(
        RepositoryRequirementKind::Depends,
        VersionScheme::Rpm,
        &format!("(native-conditional-require if {CONDITIONAL_TRIGGER})"),
    )
    .unwrap();
    conary_core::db::models::InstalledRequirementGroup::insert_groups(
        &conn,
        trove_id,
        VersionScheme::Rpm,
        &[requirement],
    )
    .unwrap();
    trove_id
}

struct NativeConditionalProviderFixture {
    db_path_string: String,
    install_root: String,
    rpm_path: String,
    conditional_trove_id: Option<i64>,
}

/// Build a dependency-free incoming RPM. When `incoming_provides_trigger` is
/// set it provides [`CONDITIONAL_TRIGGER`]; when `seed_conditional` is set the
/// installed trove `native-conditional-dependent` carries the conditional hard
/// group before the install runs.
fn native_conditional_provider_fixture(
    temp: &std::path::Path,
    incoming_provides_trigger: bool,
    seed_conditional: bool,
) -> NativeConditionalProviderFixture {
    let db_path = temp.join("conary.db");
    let install_root = temp.join("install-root");
    std::fs::create_dir_all(&install_root).unwrap();
    conary_core::db::init(&db_path).unwrap();
    let (fixture_user, fixture_group) =
        crate::commands::test_helpers::seed_unprivileged_fixture_owner(&db_path);
    let db_path_string = db_path.to_string_lossy().into_owned();
    let install_root_string = install_root.to_string_lossy().into_owned();

    let conditional_trove_id = seed_conditional
        .then(|| insert_installed_conditional(&db_path_string, "native-conditional-dependent"));

    let mut builder = rpm::PackageBuilder::new(
        "native-conditional-provider",
        "1.0.0",
        "MIT",
        "x86_64",
        "native dependency-free conditional fixture",
    );
    if incoming_provides_trigger {
        builder.provides(rpm::Dependency::any(CONDITIONAL_TRIGGER));
    }
    builder
        .with_file_contents(
            b"fixture\n".to_vec(),
            rpm::FileOptions::new("/usr/lib/native-conditional-provider/payload")
                .permissions(0o644)
                .user(fixture_user)
                .group(fixture_group),
        )
        .unwrap();
    let rpm_path = temp.join("native-conditional-provider.rpm");
    builder.build().unwrap().write_file(&rpm_path).unwrap();

    NativeConditionalProviderFixture {
        db_path_string,
        install_root: install_root_string,
        rpm_path: rpm_path.to_string_lossy().into_owned(),
        conditional_trove_id,
    }
}

/// One native install of the dependency-free provider. The explicit source
/// identity gives the strict end-state solve the exact transaction authority a
/// local artifact cannot manufacture on its own.
fn run_native_conditional_install(
    fixture: &NativeConditionalProviderFixture,
) -> Result<(), anyhow::Error> {
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
            from_source: Some("fedora-44".to_string()),
            sandbox_mode: crate::commands::SandboxMode::Always,
            yes: true,
            ..InstallOptions::default()
        },
    ))
}

/// A dependency-free incoming package that provides the capability an installed
/// package conditions on makes that implication true. With no provider for the
/// required side anywhere, the end-state solve must refuse and name the
/// installed owner. Before the early return was removed, `handle_dependencies`
/// returned `Continue` without solving.
#[test]
fn native_install_refuses_a_dependency_free_package_that_breaks_an_installed_conditional() {
    let _mount_skip = crate::commands::composefs_ops::test_mount_skip_guard();
    let temp = tempfile::tempdir().unwrap();
    let fixture = native_conditional_provider_fixture(temp.path(), true, true);
    let dependent_id = fixture
        .conditional_trove_id
        .expect("the negative fixture seeds the installed conditional");

    let error = run_native_conditional_install(&fixture).expect_err(
        "a dependency-free install that triggers an unsatisfied installed conditional must refuse",
    );
    let conflict = error
        .downcast_ref::<crate::commands::install::dependencies::DependencyConflict>()
        .expect("refusal must carry the typed dependency-conflict error");
    assert_eq!(conflict.package, "native-conditional-provider");
    assert!(
        conflict.unsatisfied.iter().any(|group| matches!(
            &group.owner,
            conary_core::resolver::sat::SatGroupOwner::Installed { trove_id, package_name }
                if *trove_id == dependent_id && package_name == "native-conditional-dependent"
        )),
        "{conflict:?}"
    );

    let conn = conary_core::db::open(&fixture.db_path_string).unwrap();
    assert!(
        Trove::find_by_name(&conn, "native-conditional-provider")
            .unwrap()
            .is_empty(),
        "a refused dependency-free package must not be persisted"
    );
}

/// Positive control on the same fixture: an incoming package that does not
/// provide the trigger leaves the installed conditional vacuous, so the solve
/// proceeds and the package installs.
#[test]
fn native_install_proceeds_when_a_dependency_free_package_leaves_an_installed_conditional_vacuous()
{
    let _mount_skip = crate::commands::composefs_ops::test_mount_skip_guard();
    let temp = tempfile::tempdir().unwrap();
    let fixture = native_conditional_provider_fixture(temp.path(), false, true);

    run_native_conditional_install(&fixture)
        .expect("an incoming package that does not trigger the installed conditional must install");

    let conn = conary_core::db::open(&fixture.db_path_string).unwrap();
    assert_eq!(
        Trove::find_by_name(&conn, "native-conditional-provider")
            .unwrap()
            .len(),
        1,
        "the dependency-free package must be persisted when the conditional is vacuous"
    );
}

/// Another transaction installs a package whose conditional hard group the
/// incoming package triggers after the pre-lock solve. The locked
/// certification must refuse with the typed requirements-change error instead
/// of committing. The positive control is
/// `native_install_proceeds_when_no_installed_conditional_appears`.
#[test]
fn native_install_refuses_when_an_installed_conditional_appears_before_the_mutation_lock() {
    let _mount_skip = crate::commands::composefs_ops::test_mount_skip_guard();
    let temp = tempfile::tempdir().unwrap();
    let fixture = native_conditional_provider_fixture(temp.path(), true, false);

    // The seam runs with the mutation lock held and installs the conditional
    // trove the pre-lock solve did not see, so the locked re-solve cannot place
    // the required capability.
    let hook_db_path = fixture.db_path_string.clone();
    crate::commands::install::dependencies::set_after_mutation_lock_hook(move || {
        insert_installed_conditional(&hook_db_path, "native-conditional-dependent");
    });

    let error = run_native_conditional_install(&fixture).expect_err(
        "the locked certification accepted a triggered installed conditional that appeared after the solve",
    );
    crate::commands::install::dependencies::clear_after_mutation_lock_hook();
    let changed = error
        .downcast_ref::<crate::commands::install::dependencies::RequirementsChanged>()
        .expect("refusal must carry the typed requirements-change error");
    assert_eq!(changed.package, "native-conditional-provider");

    let conn = conary_core::db::open(&fixture.db_path_string).unwrap();
    assert!(
        Trove::find_by_name(&conn, "native-conditional-provider")
            .unwrap()
            .is_empty(),
        "a refused dependency-free package must not be persisted"
    );
}

/// Positive control for the locked certification: the identical fixture with no
/// armed seam sees no triggered installed group, so the locked re-solve
/// succeeds and the package installs.
#[test]
fn native_install_proceeds_when_no_installed_conditional_appears_before_the_mutation_lock() {
    let _mount_skip = crate::commands::composefs_ops::test_mount_skip_guard();
    let temp = tempfile::tempdir().unwrap();
    let fixture = native_conditional_provider_fixture(temp.path(), true, false);

    crate::commands::install::dependencies::clear_after_mutation_lock_hook();
    run_native_conditional_install(&fixture)
        .expect("a dependency-free install with no triggered installed group must proceed");

    let conn = conary_core::db::open(&fixture.db_path_string).unwrap();
    assert_eq!(
        Trove::find_by_name(&conn, "native-conditional-provider")
            .unwrap()
            .len(),
        1,
        "the dependency-free package must be persisted when nothing triggers a conditional"
    );
}
