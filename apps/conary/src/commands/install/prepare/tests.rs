// apps/conary/src/commands/install/prepare/tests.rs

use super::*;
use crate::commands::install::InstallReplacement;
use conary_core::db::models::{InstallReason, InstallSource, Trove, TroveType};
use conary_core::db::schema;
use conary_core::packages::traits::PackageFile;

struct TestPackage {
    name: String,
    version: String,
    package_release: Option<String>,
    version_scheme: VersionScheme,
    architecture: Option<String>,
}

impl TestPackage {
    fn new(
        name: &str,
        version: &str,
        package_release: Option<&str>,
        version_scheme: VersionScheme,
        architecture: Option<&str>,
    ) -> Self {
        Self {
            name: name.to_string(),
            version: version.to_string(),
            package_release: package_release.map(str::to_string),
            version_scheme,
            architecture: architecture.map(str::to_string),
        }
    }
}

impl conary_core::packages::PackageFormat for TestPackage {
    fn parse(_path: &str) -> conary_core::Result<Self>
    where
        Self: Sized,
    {
        unreachable!("tests construct package instances directly")
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn package_release(&self) -> Option<&str> {
        self.package_release.as_deref()
    }

    fn version_scheme(&self) -> conary_core::repository::versioning::VersionScheme {
        self.version_scheme
    }

    fn architecture(&self) -> Option<&str> {
        self.architecture.as_deref()
    }

    fn debian_multi_arch(
        &self,
    ) -> Option<conary_core::repository::dependency_model::DebianMultiArch> {
        (self.version_scheme == VersionScheme::Debian)
            .then_some(conary_core::repository::dependency_model::DebianMultiArch::No)
    }

    fn description(&self) -> Option<&str> {
        None
    }

    fn files(&self) -> &[PackageFile] {
        &[]
    }

    fn requirements(
        &self,
    ) -> &[conary_core::repository::dependency_model::RepositoryRequirementGroup] {
        &[]
    }

    fn package_payload(&self) -> conary_core::Result<conary_core::packages::PackagePayload> {
        Ok(conary_core::packages::PackagePayload::default())
    }

    fn to_trove(&self) -> Trove {
        let mut trove = Trove::new_with_source(
            self.name.clone(),
            self.version.clone(),
            TroveType::Package,
            InstallSource::Repository,
            conary_core::repository::versioning::VersionScheme::Conary,
        );
        trove.architecture = self.architecture.clone();
        trove
    }
}

fn create_test_db() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    schema::ensure_current(&conn).unwrap();
    conn
}

fn file_test_db() -> (tempfile::TempDir, String) {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("conary.db");
    conary_core::db::init(&db_path).unwrap();
    (temp, db_path.to_string_lossy().into_owned())
}

/// Insert one installed Arch-scheme `demo` row and return it with its ID.
fn insert_arch_trove(conn: &rusqlite::Connection, version: &str, release: &str) -> Trove {
    let mut trove = Trove::new_with_source(
        "demo".to_string(),
        version.to_string(),
        TroveType::Package,
        InstallSource::Repository,
        VersionScheme::Arch,
    );
    trove.package_release = Some(release.to_string());
    trove.architecture = Some("x86_64".to_string());
    trove.insert(conn).unwrap();
    trove
}

#[test]
fn check_upgrade_status_uses_debian_version_scheme() {
    let conn = create_test_db();
    let mut trove = Trove::new_with_source(
        "demo".to_string(),
        "1.0~beta1".to_string(),
        TroveType::Package,
        InstallSource::Repository,
        VersionScheme::Debian,
    );
    trove.architecture = Some("amd64".to_string());
    trove.debian_multi_arch = Some(conary_core::repository::dependency_model::DebianMultiArch::No);
    trove.insert(&conn).unwrap();

    let pkg = TestPackage::new("demo", "1.0", None, VersionScheme::Debian, Some("amd64"));

    let result = check_upgrade_status(
        &conn,
        &pkg,
        &InstallSemantics::native_package(PackageFormatType::Deb),
        false,
        InstallIntent::PackageChange,
        None,
    )
    .unwrap();
    assert!(matches!(result, UpgradeCheck::Upgrade(_)));
}

#[test]
fn check_upgrade_status_uses_arch_version_scheme() {
    let conn = create_test_db();
    let mut trove = Trove::new_with_source(
        "demo".to_string(),
        "1.0-1".to_string(),
        TroveType::Package,
        InstallSource::Repository,
        VersionScheme::Arch,
    );
    trove.architecture = Some("x86_64".to_string());
    trove.insert(&conn).unwrap();

    let pkg = TestPackage::new("demo", "1.0-2", None, VersionScheme::Arch, Some("x86_64"));

    let result = check_upgrade_status(
        &conn,
        &pkg,
        &InstallSemantics::native_package(PackageFormatType::Arch),
        false,
        InstallIntent::PackageChange,
        None,
    )
    .unwrap();
    assert!(matches!(result, UpgradeCheck::Upgrade(_)));
}

#[test]
fn check_upgrade_status_returns_typed_already_installed_outcome() {
    let conn = create_test_db();
    let mut trove = Trove::new_with_source(
        "demo".to_string(),
        "1.0-1".to_string(),
        TroveType::Package,
        InstallSource::Repository,
        VersionScheme::Arch,
    );
    trove.architecture = Some("x86_64".to_string());
    trove.insert(&conn).unwrap();

    let pkg = TestPackage::new("demo", "1.0-1", None, VersionScheme::Arch, Some("x86_64"));

    let result = check_upgrade_status(
        &conn,
        &pkg,
        &InstallSemantics::native_package(PackageFormatType::Arch),
        false,
        InstallIntent::PackageChange,
        None,
    )
    .unwrap();
    assert!(matches!(result, UpgradeCheck::AlreadyInstalled(_)));
}

#[test]
fn check_upgrade_status_matches_cross_distro_architecture_aliases() {
    let conn = create_test_db();
    let mut trove = Trove::new_with_source(
        "demo".to_string(),
        "1.0-1".to_string(),
        TroveType::Package,
        InstallSource::Repository,
        VersionScheme::Arch,
    );
    trove.architecture = Some("x86_64".to_string());
    trove.insert(&conn).unwrap();

    let pkg = TestPackage::new("demo", "1.0-1", None, VersionScheme::Debian, Some("amd64"));

    let result = check_upgrade_status(
        &conn,
        &pkg,
        &InstallSemantics::native_package(PackageFormatType::Deb),
        false,
        InstallIntent::PackageChange,
        None,
    )
    .unwrap();
    assert!(matches!(result, UpgradeCheck::AlreadyInstalled(_)));
}

#[test]
fn check_upgrade_status_matches_architecture_independent_markers() {
    let conn = create_test_db();
    let mut trove = Trove::new_with_source(
        "demo".to_string(),
        "1.0-1".to_string(),
        TroveType::Package,
        InstallSource::Repository,
        VersionScheme::Rpm,
    );
    trove.architecture = Some("noarch".to_string());
    trove.insert(&conn).unwrap();

    let pkg = TestPackage::new("demo", "1.0-1", None, VersionScheme::Debian, Some("all"));

    let result = check_upgrade_status(
        &conn,
        &pkg,
        &InstallSemantics::native_package(PackageFormatType::Deb),
        false,
        InstallIntent::PackageChange,
        None,
    )
    .unwrap();
    assert!(matches!(result, UpgradeCheck::AlreadyInstalled(_)));
}

#[test]
fn explicit_replacement_replaces_the_selected_duplicate_row() {
    let conn = create_test_db();
    let first = insert_arch_trove(&conn, "1.0", "1");
    let second = insert_arch_trove(&conn, "1.0", "2");
    assert!(first.id.unwrap() < second.id.unwrap());

    let pkg = TestPackage::new(
        "demo",
        "1.0",
        Some("3"),
        VersionScheme::Arch,
        Some("x86_64"),
    );
    let semantics = InstallSemantics::native_package(PackageFormatType::Arch);

    let guard = InstallReplacement::Existing(second.clone());
    let explicit = check_upgrade_status(
        &conn,
        &pkg,
        &semantics,
        false,
        InstallIntent::PackageChange,
        Some(&guard),
    )
    .unwrap();
    let UpgradeCheck::Upgrade(trove) = explicit else {
        panic!("explicit replacement must upgrade the selected row");
    };
    assert_eq!(trove.id, second.id);

    // Without an explicit target the legacy first-match authority still
    // selects the earliest inserted row.
    let implicit = check_upgrade_status(
        &conn,
        &pkg,
        &semantics,
        false,
        InstallIntent::PackageChange,
        None,
    )
    .unwrap();
    let UpgradeCheck::Upgrade(trove) = implicit else {
        panic!("ordinary install must keep first-match upgrade behavior");
    };
    assert_eq!(trove.id, first.id);
}

#[test]
fn explicit_replacement_refuses_disappeared_or_changed_target() {
    let conn = create_test_db();
    let pkg = TestPackage::new(
        "demo",
        "1.0",
        Some("3"),
        VersionScheme::Arch,
        Some("x86_64"),
    );
    let semantics = InstallSemantics::native_package(PackageFormatType::Arch);

    let mut missing = Trove::new_with_source(
        "demo".to_string(),
        "1.0".to_string(),
        TroveType::Package,
        InstallSource::Repository,
        VersionScheme::Arch,
    );
    missing.id = Some(9999);
    missing.package_release = Some("1".to_string());
    missing.architecture = Some("x86_64".to_string());
    let guard = InstallReplacement::Existing(missing.clone());
    let Err(error) = check_upgrade_status(
        &conn,
        &pkg,
        &semantics,
        false,
        InstallIntent::PackageChange,
        Some(&guard),
    ) else {
        panic!("a disappeared replacement target must refuse");
    };
    assert!(error.to_string().contains("disappeared"), "{error}");

    let selected = insert_arch_trove(&conn, "1.0", "2");
    conn.execute(
        "UPDATE troves SET version = '1.1' WHERE id = ?1",
        [selected.id.unwrap()],
    )
    .unwrap();
    let guard = InstallReplacement::Existing(selected.clone());
    let Err(error) = check_upgrade_status(
        &conn,
        &pkg,
        &semantics,
        false,
        InstallIntent::PackageChange,
        Some(&guard),
    ) else {
        panic!("a changed replacement target must refuse");
    };
    assert!(
        error.to_string().contains("changed after selection"),
        "{error}"
    );
}

#[test]
fn explicit_replacement_refuses_incompatible_incoming_identity() {
    let conn = create_test_db();
    let selected = insert_arch_trove(&conn, "1.0", "1");
    let semantics = InstallSemantics::native_package(PackageFormatType::Arch);

    let wrong_name = TestPackage::new(
        "other",
        "1.0",
        Some("2"),
        VersionScheme::Arch,
        Some("x86_64"),
    );
    let guard = InstallReplacement::Existing(selected.clone());
    let Err(error) = check_upgrade_status(
        &conn,
        &wrong_name,
        &semantics,
        false,
        InstallIntent::PackageChange,
        Some(&guard),
    ) else {
        panic!("an incoming name mismatch must refuse");
    };
    assert!(error.to_string().contains("does not match"), "{error}");

    let no_architecture = TestPackage::new("demo", "1.0", Some("2"), VersionScheme::Arch, None);
    let guard = InstallReplacement::Existing(selected.clone());
    let Err(error) = check_upgrade_status(
        &conn,
        &no_architecture,
        &semantics,
        false,
        InstallIntent::PackageChange,
        Some(&guard),
    ) else {
        panic!("an incompatible incoming architecture must refuse");
    };
    assert!(error.to_string().contains("incompatible"), "{error}");
}

#[test]
fn explicit_replacement_refuses_duplicate_incoming_identity_on_other_row() {
    let conn = create_test_db();
    let first = insert_arch_trove(&conn, "1.0", "1");
    let second = insert_arch_trove(&conn, "1.0", "2");

    // The incoming identity already lives on the row that is not the selected
    // replacement target.
    let pkg = TestPackage::new(
        "demo",
        "1.0",
        Some("1"),
        VersionScheme::Arch,
        Some("x86_64"),
    );
    let guard = InstallReplacement::Existing(second.clone());
    let Err(error) = check_upgrade_status(
        &conn,
        &pkg,
        &InstallSemantics::native_package(PackageFormatType::Arch),
        false,
        InstallIntent::PackageChange,
        Some(&guard),
    ) else {
        panic!("a duplicate incoming identity on another row must refuse");
    };
    assert!(
        error
            .to_string()
            .contains("already installed as a separate record"),
        "{error}"
    );
    assert_ne!(first.id, second.id);
}

#[test]
fn planned_absent_replacement_installs_fresh_despite_unrelated_surviving_row() {
    let conn = create_test_db();
    // An unrelated row survives the earlier transaction's planned removal.
    insert_arch_trove(&conn, "1.0", "2");

    let mut removed = Trove::new_with_source(
        "demo".to_string(),
        "1.0".to_string(),
        TroveType::Package,
        InstallSource::Repository,
        VersionScheme::Arch,
    );
    removed.package_release = Some("1".to_string());
    removed.architecture = Some("x86_64".to_string());
    removed.id = Some(9999);

    let pkg = TestPackage::new(
        "demo",
        "1.0",
        Some("3"),
        VersionScheme::Arch,
        Some("x86_64"),
    );
    let guard = InstallReplacement::PlannedAbsent(removed);
    let result = check_upgrade_status(
        &conn,
        &pkg,
        &InstallSemantics::native_package(PackageFormatType::Arch),
        false,
        InstallIntent::PackageChange,
        Some(&guard),
    )
    .unwrap();
    assert!(
        matches!(result, UpgradeCheck::FreshInstall),
        "planned-absent replacement must not first-match the surviving row"
    );
}

#[test]
fn planned_absent_replacement_refuses_when_original_id_is_still_installed() {
    let conn = create_test_db();
    let installed = insert_arch_trove(&conn, "1.0", "1");

    let pkg = TestPackage::new(
        "demo",
        "1.0",
        Some("2"),
        VersionScheme::Arch,
        Some("x86_64"),
    );
    let guard = InstallReplacement::PlannedAbsent(installed);
    let Err(error) = check_upgrade_status(
        &conn,
        &pkg,
        &InstallSemantics::native_package(PackageFormatType::Arch),
        false,
        InstallIntent::PackageChange,
        Some(&guard),
    ) else {
        panic!("a planned-absent target that is still installed must refuse");
    };
    assert!(
        error
            .to_string()
            .contains("planned absent but is currently installed"),
        "{error}"
    );
}

#[test]
fn planned_absent_replacement_refuses_duplicate_incoming_identity_on_other_row() {
    let conn = create_test_db();
    insert_arch_trove(&conn, "1.0", "2");

    let mut removed = Trove::new_with_source(
        "demo".to_string(),
        "1.0".to_string(),
        TroveType::Package,
        InstallSource::Repository,
        VersionScheme::Arch,
    );
    removed.package_release = Some("1".to_string());
    removed.architecture = Some("x86_64".to_string());
    removed.id = Some(9999);

    // The incoming identity already lives on the surviving row.
    let pkg = TestPackage::new(
        "demo",
        "1.0",
        Some("2"),
        VersionScheme::Arch,
        Some("x86_64"),
    );
    let guard = InstallReplacement::PlannedAbsent(removed);
    let Err(error) = check_upgrade_status(
        &conn,
        &pkg,
        &InstallSemantics::native_package(PackageFormatType::Arch),
        false,
        InstallIntent::PackageChange,
        Some(&guard),
    ) else {
        panic!("a duplicate incoming identity on another row must refuse");
    };
    assert!(
        error
            .to_string()
            .contains("already installed as a separate record"),
        "{error}"
    );
}

#[test]
fn existing_replacement_still_refuses_missing_target() {
    let conn = create_test_db();
    insert_arch_trove(&conn, "1.0", "1");

    let mut missing = Trove::new_with_source(
        "demo".to_string(),
        "1.0".to_string(),
        TroveType::Package,
        InstallSource::Repository,
        VersionScheme::Arch,
    );
    missing.package_release = Some("1".to_string());
    missing.architecture = Some("x86_64".to_string());
    missing.id = Some(9999);

    let pkg = TestPackage::new(
        "demo",
        "1.0",
        Some("2"),
        VersionScheme::Arch,
        Some("x86_64"),
    );
    let guard = InstallReplacement::Existing(missing);
    let Err(error) = check_upgrade_status(
        &conn,
        &pkg,
        &InstallSemantics::native_package(PackageFormatType::Arch),
        false,
        InstallIntent::PackageChange,
        Some(&guard),
    ) else {
        panic!("an Existing replacement whose target disappeared must refuse");
    };
    assert!(error.to_string().contains("disappeared"), "{error}");
}

#[test]
fn root_replacement_target_does_not_propagate_to_dependency_preparation() {
    let (_temp, db_path) = file_test_db();
    let conn = conary_core::db::open(&db_path).unwrap();
    let first = insert_arch_trove(&conn, "1.0", "1");
    let second = insert_arch_trove(&conn, "1.0", "2");
    drop(conn);

    let pkg = TestPackage::new(
        "demo",
        "1.0",
        Some("3"),
        VersionScheme::Arch,
        Some("x86_64"),
    );

    let guard = InstallReplacement::Existing(second.clone());
    let root = super::super::batch::prepare_parsed_package_for_batch(
        &pkg,
        PackageFormatType::Arch,
        &db_path,
        InstallReason::Explicit,
        "Explicit update request",
        false,
        None,
        Some(&guard),
    )
    .unwrap()
    .unwrap();
    assert!(root.is_upgrade);
    assert!(matches!(
        root.replacement,
        Some(InstallReplacement::Existing(_))
    ));
    assert_eq!(
        root.old_trove.as_ref().and_then(|trove| trove.id),
        second.id
    );

    // Dependency preparation goes through the public wrapper, which never
    // carries the root replacement snapshot.
    let dependency = super::super::batch::prepare_parsed_package_for_batch(
        &pkg,
        PackageFormatType::Arch,
        &db_path,
        InstallReason::Dependency,
        "Required by demo",
        false,
        None,
        None,
    )
    .unwrap()
    .unwrap();
    assert!(dependency.is_upgrade);
    assert_eq!(
        dependency.old_trove.as_ref().and_then(|trove| trove.id),
        first.id
    );
}
