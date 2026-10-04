// apps/conary/src/commands/ccs/install/command_dry_run_root_tests/tests.rs

#![cfg(test)]

use std::collections::HashMap;

use super::command::cmd_ccs_install;
use super::test_support::{
    TestRootRegularFile, TestRootSpecialNode, ccs_regular_file, seed_test_root_layout,
    seed_test_root_layout_with_regular_files, seed_test_root_layout_with_special_nodes,
};

/// Build a signed package that ships a regular `/usr/bin/sh`. The on-disk host
/// and selected-root baselines differ only in whether that path is a symlink.
fn regular_usr_bin_sh_package(
    temp_dir: &std::path::Path,
    package_name: &str,
) -> (std::path::PathBuf, std::path::PathBuf) {
    use conary_core::ccs::{BuildResult, CcsManifest, ComponentData};

    let package_path = temp_dir.join(format!("{package_name}.ccs"));
    let content = b"#!/bin/sh\nexec true\n".to_vec();
    let content_len = content.len() as u64;
    let hash = conary_core::hash::sha256(&content);
    let files = vec![ccs_regular_file(
        "/usr/bin/sh".to_string(),
        hash.clone(),
        content_len,
        0o100755,
        "runtime".to_string(),
    )];
    let result = BuildResult {
        manifest: CcsManifest::new_minimal(package_name, "1.0.0"),
        components: HashMap::from([(
            "runtime".to_string(),
            ComponentData {
                name: "runtime".to_string(),
                files: files.clone(),
                hash: "test-runtime".to_string(),
                size: content_len,
            },
        )]),
        files: files.clone(),
        payloads: conary_core::ccs::builder::payloads_from_bounded_memory_for_tests(
            &files,
            HashMap::from([(hash, content)]),
        )
        .unwrap(),
        total_size: content_len,
        chunked: false,
        chunk_stats: None,
    };
    let trust_policy_path = super::test_support::write_signed_test_package(&result, &package_path);
    (package_path, trust_policy_path)
}

fn run_dry_run(
    package_path: &std::path::Path,
    trust_policy_path: &std::path::Path,
    db_path: &std::path::Path,
    install_root: &std::path::Path,
) -> anyhow::Result<()> {
    cmd_ccs_install(
        package_path.to_str().unwrap(),
        db_path.to_str().unwrap(),
        install_root.to_str().unwrap(),
        true,
        Some(trust_policy_path.to_string_lossy().into_owned()),
        None,
        crate::commands::SandboxMode::Always,
        true,
        false,
    )
}

#[tokio::test]
async fn ccs_dry_run_ignores_host_symlink_absent_from_selected_root() {
    let temp_dir = tempfile::tempdir().unwrap();
    let install_root = temp_dir.path().join("root");
    let db_path = temp_dir.path().join("conary.db");
    std::fs::create_dir_all(install_root.join("usr/bin")).unwrap();
    std::os::unix::fs::symlink("bash", install_root.join("usr/bin/sh")).unwrap();
    conary_core::db::init(&db_path).unwrap();
    let (package_path, policy_path) =
        regular_usr_bin_sh_package(temp_dir.path(), "preview-host-symlink");

    run_dry_run(&package_path, &policy_path, &db_path, &install_root).unwrap();

    let conn = conary_core::db::open(&db_path).unwrap();
    let troves: i64 = conn
        .query_row("SELECT COUNT(*) FROM troves", [], |row| row.get(0))
        .unwrap();
    assert_eq!(troves, 0, "dry run must not persist a trove");
    assert_eq!(
        std::fs::read_link(install_root.join("usr/bin/sh")).unwrap(),
        std::path::PathBuf::from("bash"),
        "dry run must not replace the live command-root symlink"
    );
    assert!(
        !temp_dir.path().join("selected-root-sessions").exists(),
        "dry run must not create a selected-root session"
    );
    assert!(
        !temp_dir.path().join("objects").exists(),
        "dry run must not create runtime CAS state"
    );
}

#[tokio::test]
async fn ccs_dry_run_refuses_regular_path_beneath_selected_root_symlink() {
    let temp_dir = tempfile::tempdir().unwrap();
    let install_root = temp_dir.path().join("root");
    let db_path = temp_dir.path().join("conary.db");
    std::fs::create_dir_all(&install_root).unwrap();
    conary_core::db::init(&db_path).unwrap();
    seed_test_root_layout(
        db_path.to_str().unwrap(),
        "preview-selected-symlink",
        &["/usr", "/usr/bin"],
        &[("/usr/bin/sh", "bash")],
    );
    let (package_path, policy_path) =
        regular_usr_bin_sh_package(temp_dir.path(), "preview-selected-symlink");

    let error = run_dry_run(&package_path, &policy_path, &db_path, &install_root).unwrap_err();

    let typed = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<conary_core::Error>())
        .unwrap_or_else(|| panic!("expected a typed conary_core error: {error:#}"));
    assert!(
        matches!(typed, conary_core::Error::PathTraversal(_)),
        "expected PathTraversal from the selected-root symlink: {error:#}"
    );
}

/// Build a signed package that ships a regular `/usr/bin/editor-1` plus an
/// alternatives hook. The hook preflights the target-root
/// `/usr/bin/update-alternatives`, which the baseline must provide as an
/// executable.
fn alternatives_package(
    temp_dir: &std::path::Path,
    package_name: &str,
) -> (std::path::PathBuf, std::path::PathBuf) {
    use conary_core::ccs::manifest::AlternativeHook;
    use conary_core::ccs::{BuildResult, CcsManifest, ComponentData};

    let package_path = temp_dir.join(format!("{package_name}.ccs"));
    let content = b"#!/bin/sh\nexec true\n".to_vec();
    let content_len = content.len() as u64;
    let hash = conary_core::hash::sha256(&content);
    let files = vec![ccs_regular_file(
        "/usr/bin/editor-1".to_string(),
        hash.clone(),
        content_len,
        0o100755,
        "runtime".to_string(),
    )];
    let mut manifest = CcsManifest::new_minimal(package_name, "1.0.0");
    manifest.hooks.alternatives.push(AlternativeHook {
        link: "/usr/bin/editor".to_string(),
        name: "editor".to_string(),
        path: "/usr/bin/editor-1".to_string(),
        priority: 50,
        reversible: None,
    });
    let result = BuildResult {
        manifest,
        components: HashMap::from([(
            "runtime".to_string(),
            ComponentData {
                name: "runtime".to_string(),
                files: files.clone(),
                hash: "test-runtime".to_string(),
                size: content_len,
            },
        )]),
        files: files.clone(),
        payloads: conary_core::ccs::builder::payloads_from_bounded_memory_for_tests(
            &files,
            HashMap::from([(hash, content)]),
        )
        .unwrap(),
        total_size: content_len,
        chunked: false,
        chunk_stats: None,
    };
    let trust_policy_path = super::test_support::write_signed_test_package(&result, &package_path);
    (package_path, trust_policy_path)
}

/// Run `test` again inside a user and mount namespace so the target-root
/// executable preflight sees effective root authority.
#[tokio::test]
async fn ccs_dry_run_resolves_alternatives_against_executable_baseline() {
    const TEST_NAME: &str = "commands::ccs::install::command_dry_run_root_tests::ccs_dry_run_resolves_alternatives_against_executable_baseline";
    if !crate::commands::test_helpers::run_exact_test_in_user_mount_namespace(TEST_NAME) {
        return;
    }

    let temp_dir = tempfile::tempdir().unwrap();
    let install_root = temp_dir.path().join("root");
    let db_path = temp_dir.path().join("conary.db");
    std::fs::create_dir_all(&install_root).unwrap();
    conary_core::db::init(&db_path).unwrap();
    seed_test_root_layout_with_regular_files(
        db_path.to_str().unwrap(),
        "preview-alternatives",
        &["/usr", "/usr/bin"],
        &[],
        &[TestRootRegularFile {
            path: "/usr/bin/update-alternatives",
            mode: 0o755,
        }],
    );
    let (package_path, policy_path) = alternatives_package(temp_dir.path(), "preview-alternatives");

    run_dry_run(&package_path, &policy_path, &db_path, &install_root).unwrap();

    let conn = conary_core::db::open(&db_path).unwrap();
    let troves: i64 = conn
        .query_row("SELECT COUNT(*) FROM troves", [], |row| row.get(0))
        .unwrap();
    assert_eq!(troves, 1, "dry run must not persist the previewed package");
}

/// The same alternatives package against a baseline whose target-root
/// `/usr/bin/update-alternatives` is a FIFO. A real apply refuses the FIFO, so
/// the preview must refuse it too instead of materializing an executable
/// regular placeholder.
#[tokio::test]
async fn ccs_dry_run_refuses_fifo_at_lifecycle_program_path() {
    const TEST_NAME: &str = "commands::ccs::install::command_dry_run_root_tests::ccs_dry_run_refuses_fifo_at_lifecycle_program_path";
    if !crate::commands::test_helpers::run_exact_test_in_user_mount_namespace(TEST_NAME) {
        return;
    }

    let temp_dir = tempfile::tempdir().unwrap();
    let install_root = temp_dir.path().join("root");
    let db_path = temp_dir.path().join("conary.db");
    std::fs::create_dir_all(&install_root).unwrap();
    conary_core::db::init(&db_path).unwrap();
    seed_test_root_layout_with_special_nodes(
        db_path.to_str().unwrap(),
        "preview-alternatives-fifo",
        &["/usr", "/usr/bin"],
        &[],
        &[],
        &[TestRootSpecialNode {
            path: "/usr/bin/update-alternatives",
            kind: conary_core::payload::PayloadNodeKind::Fifo,
            mode: libc::S_IFIFO | 0o755,
        }],
    );
    let (package_path, policy_path) =
        alternatives_package(temp_dir.path(), "preview-alternatives-fifo");

    let error = run_dry_run(&package_path, &policy_path, &db_path, &install_root).unwrap_err();

    let typed = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<conary_core::Error>())
        .unwrap_or_else(|| panic!("expected a typed conary_core error: {error:#}"));
    match typed {
        conary_core::Error::ScriptletExecution {
            kind: conary_core::scriptlet::ScriptletFailureKind::ProgramUnavailable,
            ..
        } => {}
        other => panic!(
            "expected ProgramUnavailable for the FIFO lifecycle program, got {other:?}: {error:#}"
        ),
    }
}

/// A real runtime database plus its boot-visible runtime root. The fixture
/// installs one file whose parent directory has no installed database row, so
/// only a generation artifact can supply that parent closure.
struct PreviewRuntimeFixture {
    temp: tempfile::TempDir,
    db_path: std::path::PathBuf,
}

impl PreviewRuntimeFixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("conary.db");
        conary_core::db::init(&db_path).unwrap();
        let conn = conary_core::db::open(&db_path).unwrap();
        crate::commands::test_helpers::persist_test_host_capabilities(&conn);
        drop(conn);
        Self { temp, db_path }
    }

    fn runtime_root(&self) -> conary_core::runtime_root::ConaryRuntimeRoot {
        conary_core::runtime_root::ConaryRuntimeRoot::from_db_path(self.db_path.clone())
    }

    /// Install `/sbin/init` without inserting its `/sbin` parent directory row.
    fn seed_installed_file_without_parent_row(&self) {
        use conary_core::db::models::{FileEntry, Trove, TroveType};
        use conary_core::payload::{
            PayloadContentAuthority, PayloadIdentity, PayloadNode, ResolvedPayloadNode,
        };
        use conary_core::repository::versioning::VersionScheme;

        let content = b"installed fixture\n";
        let runtime_root = self.runtime_root();
        let sha256 = conary_core::filesystem::CasStore::new(runtime_root.objects_dir())
            .unwrap()
            .store(content)
            .unwrap();

        let conn = conary_core::db::open(&self.db_path).unwrap();
        let mut trove = Trove::new(
            "preview-unclaimed-parent".to_string(),
            "1.0.0".to_string(),
            TroveType::Package,
            VersionScheme::Conary,
        );
        trove.architecture =
            Some(conary_core::ccs::manifest::DEFAULT_CONARY_ARCHITECTURE.to_string());
        let trove_id = trove.insert(&conn).unwrap();

        let mut node = PayloadNode::regular(0o755);
        node.user = PayloadIdentity::Numeric {
            id: u64::from(unsafe { libc::geteuid() }),
        };
        node.group = PayloadIdentity::Numeric {
            id: u64::from(unsafe { libc::getegid() }),
        };
        let mut entry = FileEntry::new(
            "/sbin/init".to_string(),
            ResolvedPayloadNode::from_numeric_source(node).unwrap(),
            Some(PayloadContentAuthority {
                sha256,
                size: content.len() as u64,
            }),
            trove_id,
        );
        entry.insert(&conn).unwrap();
        assert!(
            FileEntry::find_by_path(&conn, "/sbin").unwrap().is_none(),
            "the fixture must leave /sbin unclaimed as a parent directory"
        );
    }

    /// Publish generation 1 and point `/current` at it. The artifact carries
    /// `/sbin` and `/sbin/init`.
    fn activate_generation(&self) {
        crate::commands::test_helpers::create_active_test_generation(&self.db_path, 1);
        assert!(
            conary_core::generation::mount::current_generation(self.runtime_root().root())
                .unwrap()
                .is_some(),
            "the fixture must have an active generation"
        );
    }
}

fn probe_regular_file(path: &str, content: &[u8], mode: u32) -> conary_core::ccs::FileEntry {
    use conary_core::payload::{PayloadContentAuthority, PayloadIdentity, PayloadNode};

    let mut node = PayloadNode::regular(mode & 0o7777);
    node.user = PayloadIdentity::Numeric {
        id: u64::from(unsafe { libc::geteuid() }),
    };
    node.group = PayloadIdentity::Numeric {
        id: u64::from(unsafe { libc::getegid() }),
    };
    conary_core::ccs::FileEntry {
        path: path.to_string(),
        node,
        content: Some(PayloadContentAuthority {
            sha256: conary_core::hash::sha256(content),
            size: content.len() as u64,
        }),
        component: "runtime".to_string(),
        chunks: None,
    }
}

fn signed_probe_package(temp_dir: &std::path::Path, name: &str) -> conary_core::ccs::CcsPackage {
    use conary_core::ccs::builder::write_signed_current_ccs_package;
    use conary_core::ccs::{BuildResult, CcsManifest, ComponentData, SigningKeyPair};

    let package_path = temp_dir.join(format!("{name}.ccs"));
    let content = b"probe\n".to_vec();
    let files = vec![probe_regular_file(
        "/usr/bin/preview-runtime-root",
        &content,
        0o100755,
    )];
    let result = BuildResult {
        manifest: CcsManifest::new_minimal(name, "1.0.0"),
        components: HashMap::from([(
            "runtime".to_string(),
            ComponentData {
                name: "runtime".to_string(),
                files: files.clone(),
                hash: "test-runtime".to_string(),
                size: content.len() as u64,
            },
        )]),
        files: files.clone(),
        payloads: conary_core::ccs::builder::payloads_from_bounded_memory_for_tests(
            &files,
            HashMap::from([(conary_core::hash::sha256(&content), content)]),
        )
        .unwrap(),
        total_size: 0,
        chunked: false,
        chunk_stats: None,
    };
    let signing_key = SigningKeyPair::generate().with_key_id("preview-runtime-root-test");
    write_signed_current_ccs_package(&result, &package_path, &signing_key, true).unwrap();

    let verification = crate::commands::install::verify_ccs_package_authority(
        temp_dir.join("conary.db").to_str().unwrap(),
        &package_path,
        &crate::commands::install::CcsEnvelopeAuthority::ExactKey(signing_key.public_key_base64()),
        None,
    )
    .unwrap();
    conary_core::ccs::CcsPackage::from_verified_archive(
        package_path.to_str().unwrap(),
        &verification,
    )
    .unwrap()
}

/// Run the same direct CCS transaction the update planner drives: a disposable
/// projection bound to its real runtime database.
fn run_preview_dry_run(
    fixture: &PreviewRuntimeFixture,
    package: &conary_core::ccs::CcsPackage,
) -> anyhow::Result<()> {
    let real_conn = conary_core::db::open(&fixture.db_path).unwrap();
    let preview = crate::commands::install::preview::PreviewDatabase::new(
        &real_conn,
        fixture.db_path.to_str().unwrap(),
    )
    .unwrap();
    drop(real_conn);

    let install_root = fixture.temp.path().join("install-root");
    std::fs::create_dir_all(&install_root).unwrap();
    let mut conn = conary_core::db::open(preview.path()).unwrap();
    crate::commands::install::install_ccs_package_transactionally(
        &mut conn,
        package,
        crate::commands::install::CcsTransactionInstallOptions {
            preview: Some(&preview),
            db_path: preview.path(),
            root: install_root.to_str().unwrap(),
            dry_run: true,
            defer_generation: false,
            quiet: true,
            sandbox_mode: conary_core::scriptlet::SandboxMode::Always,
            allow_downgrade: false,
            intent: crate::commands::install::InstallIntent::PackageChange,
            reinstall: false,
            selection_reason: None,
            selected_manifest_components: None,
            repository_provenance: None,
            requested_source_identity: None,
            replacement: None,
            certified_outgoing: None,
            certified_requirements: None,
        },
    )
    .map(|_| ())
}

/// Regression: the installed database omits a parent directory row that the
/// active generation artifact carries. The preview must read that artifact
/// through the real runtime root rather than project an empty stand-in.
#[test]
fn preview_dry_run_baseline_reads_the_real_runtime_current_generation() {
    let fixture = PreviewRuntimeFixture::new();
    fixture.seed_installed_file_without_parent_row();
    fixture.activate_generation();
    let package = signed_probe_package(fixture.temp.path(), "preview-current-generation");

    run_preview_dry_run(&fixture, &package)
        .expect("the real runtime generation supplies the package-unclaimed /sbin parent closure");
}

/// Control for the regression: the identical fixture has no generation, so the
/// preview keeps the typed database-projection behavior and cannot invent the
/// missing parent directory.
#[test]
fn preview_dry_run_baseline_without_a_generation_uses_database_projection() {
    let fixture = PreviewRuntimeFixture::new();
    fixture.seed_installed_file_without_parent_row();
    let package = signed_probe_package(fixture.temp.path(), "preview-database-projection");

    let error = run_preview_dry_run(&fixture, &package)
        .expect_err("database projection cannot resolve the unclaimed /sbin parent");

    let typed = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<conary_core::Error>())
        .unwrap_or_else(|| panic!("expected a typed conary_core error: {error:#}"));
    assert!(
        matches!(typed, conary_core::Error::InvalidPath(_)),
        "expected InvalidPath from the database-projection parent closure, got {typed:?}: {error:#}"
    );
}
