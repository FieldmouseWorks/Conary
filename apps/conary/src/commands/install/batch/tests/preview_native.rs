// apps/conary/src/commands/install/batch/tests/preview_native.rs

use super::*;
use crate::commands::install::preview::PreviewDatabase;
use conary_core::ccs::native_lifecycle::{
    NATIVE_LIFECYCLE_SCHEMA_REVISION, NATIVE_LIFECYCLE_SCHEMA_V1, NativeLifecycleBundle,
    ScriptletFidelity, SourceFormat, VersionScheme as LifecycleVersionScheme,
};
use conary_core::ccs::native_transaction::{DebPackageState, NativePackageIdentity};
use conary_core::db::models::{InstalledNativeLifecycleBundle, NativeLifecycleResidualState};
use conary_core::repository::dependency_model::{
    DebianMultiArch, PackageRelationRemovalMode, RepositoryRequirementKind,
};
use conary_core::repository::versioning::VersionScheme;
use conary_core::transaction::{PackageRelationIncomingIdentity, PackageRelationRemoval};

#[test]
fn preview_preserves_debian_removal_completion_and_disappearance() {
    for transfer_all_paths in [false, true] {
        let (_temp, db_path) = crate::commands::test_helpers::create_test_db();
        let conn = conary_core::db::open(&db_path).unwrap();
        let mut old = Trove::new(
            "deb-obsolete".into(),
            "1.0-1".into(),
            TroveType::Package,
            VersionScheme::Debian,
        );
        old.architecture = Some("amd64".into());
        old.debian_multi_arch = Some(DebianMultiArch::No);
        let old_id = old.insert(&conn).unwrap();
        let old_path = "/usr/share/deb-obsolete/data";
        installed_regular_file(old_path, b"old", 0o644, old_id)
            .insert(&conn)
            .unwrap();
        let bundle = NativeLifecycleBundle {
            schema: NATIVE_LIFECYCLE_SCHEMA_V1.into(),
            schema_revision: NATIVE_LIFECYCLE_SCHEMA_REVISION,
            source_format: SourceFormat::Deb,
            source_family: "debian".into(),
            source_profile: Some("ubuntu-26.04".into()),
            source_release: Some("26.04".into()),
            source_arch: Some("amd64".into()),
            source_package: old.name.clone(),
            source_version: old.version.clone(),
            source_checksum: None,
            version_scheme: LifecycleVersionScheme::Deb,
            conversion_tool: "fixture".into(),
            conversion_tool_version: "1".into(),
            conversion_policy: "fixture".into(),
            evidence_digest: None,
            scriptlet_fidelity: ScriptletFidelity::NativeLifecycle,
            entries: Vec::new(),
        };
        InstalledNativeLifecycleBundle::new(old_id, None, &bundle)
            .unwrap()
            .insert_or_replace(&conn)
            .unwrap();
        let mut package = prepared_test_package(
            "replacement",
            if transfer_all_paths {
                old_path
            } else {
                "/usr/share/replacement/data"
            },
            b"new",
        );
        package.relation_removals.push(PackageRelationRemoval {
            trove_id: old_id,
            package_name: old.name.clone(),
            package_version: old.version.clone(),
            package_architecture: old.architecture.clone(),
            triggering_incoming: PackageRelationIncomingIdentity {
                transaction_index: 0,
                package_name: package.name.clone(),
                package_version: package.version.clone(),
                package_architecture: package.architecture.clone(),
            },
            incoming_packages: vec![package.name.clone()],
            ownership_transfer_packages: vec![package.name.clone()],
            kind: RepositoryRequirementKind::Obsolete,
            mode: PackageRelationRemovalMode::OwnershipTransfer,
            native_text: Some("deb-obsolete < 2".into()),
        });
        let before = crate::commands::test_helpers::database_rows(&conn);
        let projection = PreviewDatabase::new(&conn, &db_path).unwrap();
        projection.project(&[package]).unwrap();
        let projected = conary_core::db::open(projection.path()).unwrap();
        assert!(Trove::find_by_id(&projected, old_id).unwrap().is_none());
        let residual = NativeLifecycleResidualState::find_exact(
            &projected,
            SourceFormat::Deb.as_str(),
            &NativePackageIdentity::new(&old.name, &old.version, old.architecture.as_deref()),
        )
        .unwrap();
        if transfer_all_paths {
            assert!(
                residual.is_none(),
                "disappearance must leave no residual authority"
            );
        } else {
            let residual =
                residual.expect("ordinary removal must retain Debian config-files state");
            assert_eq!(residual.lifecycle_state, DebPackageState::ConfigFiles);
            assert_eq!(residual.bundle().unwrap(), bundle);
        }
        assert_eq!(crate::commands::test_helpers::database_rows(&conn), before);
    }
}
