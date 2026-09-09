// apps/conary/src/commands/install/conversion/tests/dependencies/hook_preflight.rs

use super::*;
use conary_core::ccs::manifest::{Service, ServiceAction};
use conary_core::repository::dependency_model::{
    RepositoryRequirementClause, RepositoryRequirementGroup, RepositoryRequirementKind,
};
use conary_core::repository::versioning::VersionScheme;

#[tokio::test]
async fn ccs_dependency_preview_preserves_incoming_hook_preflight_without_mutation() {
    // The direct root path already preflights. The same root and dependency
    // requirements must refuse in an atomic dependency preview as well.
    for (with_dependency, hook_on_dependency) in [(false, false), (true, false), (true, true)] {
        let (temp, db_path) = crate::commands::test_helpers::create_test_db();
        let root = temp.path().join("selected-root");
        std::fs::create_dir_all(&root).unwrap();
        let conn = conary_core::db::open(&db_path).unwrap();
        let key = crate::commands::ccs::load_or_create_local_dev_key().unwrap();
        let mut dependency = CcsManifest::new_minimal("hook-dependency", "1.0.0");
        let mut incoming = CcsManifest::new_minimal("hook-root", "1.0.0");
        dependency.package.platform.as_mut().unwrap().arch = Some("x86_64".into());
        incoming.package.platform.as_mut().unwrap().arch = Some("x86_64".into());
        if with_dependency {
            incoming.requirements = vec![RepositoryRequirementGroup::simple(
                RepositoryRequirementKind::Depends,
                RepositoryRequirementClause::name_only("hook-dependency".into()),
            )];
        }
        let hooks = if hook_on_dependency {
            &mut dependency.hooks
        } else {
            &mut incoming.hooks
        };
        hooks.services.push(Service {
            name: "fixture.service".into(),
            action: ServiceAction::Enable,
            reversible: Some(true),
        });
        let dependency_path = write_signed_ccs_fixture(
            temp.path(),
            "hook-dependency",
            dependency,
            "/usr/share/hook-dependency/data",
            b"dependency",
            &key,
        );
        let bytes = std::fs::read(&dependency_path).unwrap();
        let repository_id = crate::commands::test_helpers::insert_test_static_ccs_repository(
            &conn,
            "hook-repository",
            "https://example.invalid/fixture",
        );
        let mut candidate = conary_core::db::models::RepositoryPackage::new(
            repository_id,
            "hook-dependency".into(),
            "1.0.0".into(),
            VersionScheme::Conary,
            hash::sha256(&bytes),
            bytes.len() as i64,
            dependency_path.to_string_lossy().into_owned(),
        );
        candidate.architecture = Some("x86_64".into());
        let id = candidate.insert(&conn).unwrap();
        conary_core::db::models::RepositoryProvide::new(
            id,
            "hook-dependency".into(),
            Some("1.0.0".into()),
            "package".into(),
            None,
            VersionScheme::Conary,
        )
        .insert(&conn)
        .unwrap();
        let incoming_path = write_signed_ccs_fixture(
            temp.path(),
            "hook-root",
            incoming,
            "/usr/share/hook-root/data",
            b"root",
            &key,
        );
        let before = crate::commands::test_helpers::database_rows(&conn);
        let mut report = super::super::super::super::report::InstallReport::default();
        let error = install_ccs_artifact_with_report(
            CcsArtifactInstallOptions {
                ccs_path: incoming_path.to_str().unwrap(),
                db_path: &db_path,
                root: root.to_str().unwrap(),
                dry_run: true,
                sandbox_mode: SandboxMode::Always,
                no_deps: false,
                allow_downgrade: false,
                intent: InstallIntent::PackageChange,
                yes: true,
                envelope_authority: CcsEnvelopeAuthority::LocalDev,
                repository_provenance: None,
                requested_source_identity: None,
                resolution_policy: test_resolution_policy(),
            },
            &mut report,
        )
        .await
        .expect_err("missing service capability inventory must refuse preview");
        assert!(
            format!("{error:#}").contains("CCS lifecycle host capability preflight failed"),
            "dependency={with_dependency}, hook_on_dependency={hook_on_dependency}: {error:#}"
        );
        assert!(report.planned.is_empty());
        assert!(report.commits.is_empty());
        assert_eq!(crate::commands::test_helpers::database_rows(&conn), before);
        assert!(std::fs::read_dir(&root).unwrap().next().is_none());
        assert!(!conary_core::db::paths::objects_dir(&db_path).exists());
    }
}
