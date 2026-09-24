// apps/conary/src/commands/install/transaction/tests.rs

#![cfg(test)]

use super::*;
use crate::commands::ccs::cmd_ccs_install;
use conary_core::db::models::{ProvideEntry, Trove, TroveType};
use conary_core::packages::payload::{PackagePayload, PackagePayloadFile};
use conary_core::packages::traits::{PackageFile, PackageFormat};
use conary_core::payload::{PayloadIdentity, PayloadNode, PayloadNodeKind, PayloadTimestamp};
use conary_core::repository::dependency_model::{
    CapabilityProvenance, ProvideArchitectureQualifier, ProvideVersionRelation, ProvidedCapability,
    RepositoryCapabilityKind, SourcePackageFormat,
};
use conary_core::repository::versioning::VersionScheme;
use std::collections::HashMap;

/// Minimal `PackageFormat` for exercising declared-provide persistence without a
/// native archive on disk.
struct DeclaredProvideTestPackage {
    name: &'static str,
    version: &'static str,
    version_scheme: VersionScheme,
    payload_files: Vec<PackageFile>,
    declared_provides: Vec<ProvidedCapability>,
}

impl PackageFormat for DeclaredProvideTestPackage {
    fn parse(_path: &str) -> conary_core::Result<Self> {
        Err(conary_core::Error::ParseError(
            "declared-provide test package is constructed directly".to_string(),
        ))
    }

    fn name(&self) -> &str {
        self.name
    }

    fn version(&self) -> &str {
        self.version
    }

    fn version_scheme(&self) -> VersionScheme {
        self.version_scheme
    }

    fn architecture(&self) -> Option<&str> {
        None
    }

    fn description(&self) -> Option<&str> {
        None
    }

    fn files(&self) -> &[PackageFile] {
        &self.payload_files
    }

    fn requirements(
        &self,
    ) -> &[conary_core::repository::dependency_model::RepositoryRequirementGroup] {
        &[]
    }

    fn resolution_capabilities(&self) -> conary_core::Result<Vec<ProvidedCapability>> {
        Ok(self.declared_provides.clone())
    }

    fn package_payload(&self) -> conary_core::Result<PackagePayload> {
        Ok(PackagePayload::default())
    }

    fn to_trove(&self) -> Trove {
        Trove::new(
            self.name.to_string(),
            self.version.to_string(),
            TroveType::Package,
            self.version_scheme,
        )
    }
}

fn package_self_provide(name: &str, version: &str, scheme: VersionScheme) -> ProvidedCapability {
    ProvidedCapability {
        kind: RepositoryCapabilityKind::PackageName,
        name: name.to_string(),
        version: Some(version.to_string()),
        version_relation: Some(ProvideVersionRelation::Equal),
        version_scheme: scheme,
        architecture_qualifier: ProvideArchitectureQualifier::Implicit,
        provenance: CapabilityProvenance::ExactIdentity,
    }
}

fn declared_file_provide(name: &str, scheme: VersionScheme) -> ProvidedCapability {
    ProvidedCapability {
        kind: RepositoryCapabilityKind::File,
        name: name.to_string(),
        version: None,
        version_relation: None,
        version_scheme: scheme,
        architecture_qualifier: ProvideArchitectureQualifier::Implicit,
        provenance: CapabilityProvenance::SourceDeclared {
            format: SourcePackageFormat::for_version_scheme(scheme),
            record_index: 0,
        },
    }
}

fn payload_file(path: &str) -> PackageFile {
    PackageFile {
        path: path.to_string(),
        node: PayloadNode::regular(0o755),
        content: None,
    }
}

fn extracted_symlink(path: &str, target: &str) -> PackagePayloadFile {
    let node = PayloadNode {
        kind: PayloadNodeKind::Symlink {
            target: target.to_string(),
        },
        mode: libc::S_IFLNK | 0o777,
        user: PayloadIdentity::Numeric { id: 0 },
        group: PayloadIdentity::Numeric { id: 0 },
        mtime: PayloadTimestamp::UNIX_EPOCH,
        xattrs: Default::default(),
    };
    PackagePayloadFile::new(path.to_string(), node, None, None).unwrap()
}

fn persist_and_read_file_provides(
    package: &dyn PackageFormat,
    extracted_files: &[PackagePayloadFile],
) -> Vec<ProvideEntry> {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("conary.db");
    let db_path_str = db_path.to_str().unwrap();
    conary_core::db::init(db_path_str).unwrap();
    let mut conn = conary_core::db::open(db_path_str).unwrap();
    let trove_id = {
        let tx = conn.transaction().unwrap();
        let mut trove = package.to_trove();
        let trove_id = trove.insert(&tx).unwrap();
        persist_package_provides(
            &tx,
            trove_id,
            package,
            InstallSemantics::ccs(package.version_scheme()),
            extracted_files,
        )
        .unwrap();
        tx.commit().unwrap();
        trove_id
    };
    ProvideEntry::find_by_trove_and_kind(&conn, trove_id, RepositoryCapabilityKind::File).unwrap()
}

#[test]
fn declared_file_provide_matching_uninstalled_payload_path_is_dropped() {
    let scheme = VersionScheme::Conary;
    let package = DeclaredProvideTestPackage {
        name: "component-selection-fixture",
        version: "1.0.0",
        version_scheme: scheme,
        payload_files: vec![
            payload_file("/bin/sh"),
            payload_file("/usr/share/doc/component-selection-fixture/README"),
        ],
        declared_provides: vec![
            package_self_provide("component-selection-fixture", "1.0.0", scheme),
            declared_file_provide("/bin/sh", scheme),
        ],
    };

    let docs_only = persist_and_read_file_provides(
        &package,
        &[extracted_symlink(
            "/usr/share/doc/component-selection-fixture/README",
            "README",
        )],
    );
    assert!(
        docs_only
            .iter()
            .all(|provide| provide.capability != "/bin/sh"),
        "a declared File provide for a shipped payload path the selected components skipped must not persist"
    );

    let runtime_installed =
        persist_and_read_file_provides(&package, &[extracted_symlink("/bin/sh", "dash")]);
    assert!(
        runtime_installed
            .iter()
            .any(|provide| provide.capability == "/bin/sh"),
        "installing the component that ships the provided path must persist the declared File provide"
    );
}

#[test]
fn declared_file_provide_absent_from_payload_is_retained() {
    let scheme = VersionScheme::Rpm;
    let package = DeclaredProvideTestPackage {
        name: "rpm-like-bash",
        version: "5.2.26-1",
        version_scheme: scheme,
        // An RPM header may declare a path the payload materializes elsewhere.
        payload_files: vec![payload_file("/usr/bin/sh")],
        declared_provides: vec![
            package_self_provide("rpm-like-bash", "5.2.26-1", scheme),
            declared_file_provide("/bin/sh", scheme),
        ],
    };

    let provides =
        persist_and_read_file_provides(&package, &[extracted_symlink("/usr/bin/sh", "bash")]);
    assert!(
        provides
            .iter()
            .any(|provide| provide.capability == "/bin/sh"),
        "a source-format File provide for a path the package does not ship must survive install"
    );
}

// ---------------------------------------------------------------------------
// End-to-end CCS component selection.
// ---------------------------------------------------------------------------

struct InstallFixtureRoot {
    db_path: std::path::PathBuf,
    install_root: std::path::PathBuf,
}

fn install_fixture_root(base: &std::path::Path, name: &str) -> InstallFixtureRoot {
    let dir = base.join(format!("{name}-root"));
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("conary.db");
    let install_root = dir.join("root");
    std::fs::create_dir_all(&install_root).unwrap();
    let db_path_str = db_path.to_str().unwrap();
    conary_core::db::init(db_path_str).unwrap();
    stage_test_boot_assets(&dir);
    seed_test_init_trove(db_path_str, &dir);
    InstallFixtureRoot {
        db_path,
        install_root,
    }
}

fn installed_file_provides(db_path: &str) -> Vec<ProvideEntry> {
    let conn = conary_core::db::open(db_path).unwrap();
    let troves = Trove::find_by_name(&conn, "file-provide-selection").unwrap();
    assert_eq!(troves.len(), 1, "fixture trove must be installed once");
    let trove_id = troves[0].id.expect("installed trove has a database id");
    ProvideEntry::find_by_trove_and_kind(&conn, trove_id, RepositoryCapabilityKind::File).unwrap()
}

#[tokio::test]
async fn ccs_component_selection_does_not_persist_uninstalled_declared_file_provide() {
    use conary_core::ccs::{BuildResult, CcsManifest, ComponentData};
    use conary_core::hash;

    let _mount_guard = crate::commands::composefs_ops::test_mount_skip_guard();
    let temp_dir = tempfile::tempdir().unwrap();
    let package_path = temp_dir.path().join("file-provide-selection.ccs");

    let sh_content = b"#!/bin/sh\nexec true\n".to_vec();
    let sh_hash = hash::sha256(&sh_content);
    let readme_content = b"component selection fixture\n".to_vec();
    let readme_hash = hash::sha256(&readme_content);

    let runtime_file = ccs_regular_file(
        "/bin/sh".to_string(),
        sh_hash.clone(),
        sh_content.len() as u64,
        0o100755,
        "runtime".to_string(),
    );
    let docs_file = ccs_regular_file(
        "/usr/share/doc/file-provide-selection/README".to_string(),
        readme_hash.clone(),
        readme_content.len() as u64,
        0o100644,
        "docs".to_string(),
    );
    let files = vec![runtime_file.clone(), docs_file.clone()];

    let mut manifest = CcsManifest::new_minimal("file-provide-selection", "1.0.0");
    manifest.components.default = vec!["runtime".to_string()];
    manifest.provides.files = vec!["/bin/sh".to_string()];

    let result = BuildResult {
        manifest,
        components: HashMap::from([
            (
                "runtime".to_string(),
                ComponentData {
                    name: "runtime".to_string(),
                    files: vec![runtime_file],
                    hash: "runtime".to_string(),
                    size: sh_content.len() as u64,
                },
            ),
            (
                "docs".to_string(),
                ComponentData {
                    name: "docs".to_string(),
                    files: vec![docs_file],
                    hash: "docs".to_string(),
                    size: readme_content.len() as u64,
                },
            ),
        ]),
        files: files.clone(),
        payloads: conary_core::ccs::builder::payloads_from_bounded_memory_for_tests(
            &files,
            HashMap::from([(sh_hash, sh_content), (readme_hash, readme_content)]),
        )
        .unwrap(),
        total_size: 0,
        chunked: false,
        chunk_stats: None,
    };
    let trust_policy_path = write_signed_test_package(&result, &package_path);
    let trust_policy = trust_policy_path.to_string_lossy().into_owned();

    let docs_root = install_fixture_root(temp_dir.path(), "docs");
    cmd_ccs_install(
        package_path.to_str().unwrap(),
        docs_root.db_path.to_str().unwrap(),
        docs_root.install_root.to_str().unwrap(),
        false,
        Some(trust_policy.clone()),
        Some(vec!["docs".to_string()]),
        crate::commands::SandboxMode::Always,
        true,
        false,
    )
    .unwrap();
    let docs_provides = installed_file_provides(docs_root.db_path.to_str().unwrap());
    assert!(
        docs_provides
            .iter()
            .all(|provide| provide.capability != "/bin/sh"),
        "installing only the docs component must not persist a File provide for the runtime-only payload path"
    );

    let runtime_root = install_fixture_root(temp_dir.path(), "runtime");
    cmd_ccs_install(
        package_path.to_str().unwrap(),
        runtime_root.db_path.to_str().unwrap(),
        runtime_root.install_root.to_str().unwrap(),
        false,
        Some(trust_policy),
        Some(vec!["runtime".to_string()]),
        crate::commands::SandboxMode::Always,
        true,
        false,
    )
    .unwrap();
    let runtime_provides = installed_file_provides(runtime_root.db_path.to_str().unwrap());
    assert!(
        runtime_provides
            .iter()
            .any(|provide| provide.capability == "/bin/sh"),
        "installing the component that ships the provided path must persist the declared File provide"
    );
}

fn write_signed_test_package(
    result: &conary_core::ccs::BuildResult,
    package_path: &std::path::Path,
) -> std::path::PathBuf {
    let signing_key =
        conary_core::ccs::signing::SigningKeyPair::generate().with_key_id("test-authority");
    conary_core::ccs::builder::write_signed_current_ccs_package(
        result,
        package_path,
        &signing_key,
        false,
    )
    .unwrap();
    let policy_path = package_path.with_extension("trust-policy.toml");
    std::fs::write(
        &policy_path,
        format!(
            "trusted_keys = [\"{}\"]\nrequire_timestamp = false\n",
            signing_key.public_key_base64()
        ),
    )
    .unwrap();
    policy_path
}

fn ccs_regular_file(
    path: impl Into<String>,
    sha256: impl Into<String>,
    size: u64,
    mode: u32,
    component: impl Into<String>,
) -> conary_core::ccs::FileEntry {
    let mut node = conary_core::payload::PayloadNode::regular(mode & 0o7777);
    node.user = conary_core::payload::PayloadIdentity::Numeric {
        id: u64::from(unsafe { libc::geteuid() }),
    };
    node.group = conary_core::payload::PayloadIdentity::Numeric {
        id: u64::from(unsafe { libc::getegid() }),
    };
    conary_core::ccs::FileEntry {
        path: path.into(),
        node,
        content: Some(conary_core::payload::PayloadContentAuthority {
            sha256: sha256.into(),
            size,
        }),
        component: component.into(),
        chunks: None,
    }
}

fn stage_test_boot_assets(root: &std::path::Path) {
    let conn = conary_core::db::open(root.join("conary.db")).unwrap();
    crate::commands::test_helpers::persist_test_host_capabilities(&conn);
    drop(conn);

    let kernel_version = "test-kernel";
    let boot_root = root.join("boot");
    std::fs::create_dir_all(boot_root.join("EFI/BOOT")).unwrap();
    std::fs::write(
        boot_root.join(format!("vmlinuz-{kernel_version}")),
        b"test-kernel",
    )
    .unwrap();
    std::fs::write(
        boot_root.join(format!("initramfs-{kernel_version}.img")),
        b"test-initramfs",
    )
    .unwrap();
    std::fs::write(boot_root.join("EFI/BOOT/BOOTX64.EFI"), b"test-efi").unwrap();
}

fn seed_test_init_trove(db_path: &str, db_dir: &std::path::Path) {
    use conary_core::db::models::{
        Changeset, ChangesetStatus, Component, FileEntry, ProvideEntry, Trove, TroveType,
    };
    use conary_core::payload::{
        PayloadContentAuthority, PayloadIdentity, PayloadNode, PayloadNodeKind, PayloadTimestamp,
        ResolvedPayloadNode,
    };

    let cas = conary_core::filesystem::CasStore::new(db_dir.join("objects")).unwrap();
    let init_content = b"#!/bin/sh\nexec true\n";
    let init_hash = cas.store(init_content).unwrap();
    let init_size = i64::try_from(init_content.len()).unwrap();
    let mut conn = conary_core::db::open(db_path).unwrap();

    conary_core::db::transaction(&mut conn, |tx| {
        let mut changeset = Changeset::new("Install test-init-1.0.0".to_string());
        let changeset_id = changeset.insert(tx)?;

        let mut trove = Trove::new(
            "test-init".to_string(),
            "1.0.0".to_string(),
            TroveType::Package,
            conary_core::repository::versioning::VersionScheme::Conary,
        );
        trove.architecture =
            Some(conary_core::ccs::manifest::DEFAULT_CONARY_ARCHITECTURE.to_string());
        trove.installed_by_changeset_id = Some(changeset_id);
        let trove_id = trove.insert(tx)?;

        let mut component = Component::new(trove_id, "runtime".to_string());
        let component_id = component.insert(tx)?;

        tx.execute(
            "INSERT OR IGNORE INTO file_contents (sha256_hash, content_path, size) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                &init_hash,
                format!("objects/{}/{}", &init_hash[0..2], &init_hash[2..]),
                init_size
            ],
        )?;

        let current_identity = || {
            PayloadNode {
                kind: PayloadNodeKind::Directory,
                mode: libc::S_IFDIR | 0o755,
                user: PayloadIdentity::Numeric {
                    id: u64::from(unsafe { libc::geteuid() }),
                },
                group: PayloadIdentity::Numeric {
                    id: u64::from(unsafe { libc::getegid() }),
                },
                mtime: PayloadTimestamp::UNIX_EPOCH,
                xattrs: Default::default(),
            }
        };
        let mut sbin = FileEntry::new(
            "/sbin".to_string(),
            ResolvedPayloadNode::from_numeric_source(current_identity()).unwrap(),
            None,
            trove_id,
        );
        sbin.component_id = Some(component_id);
        sbin.insert(tx)?;

        let mut init_node = PayloadNode::regular(0o755);
        init_node.user = PayloadIdentity::Numeric {
            id: u64::from(unsafe { libc::geteuid() }),
        };
        init_node.group = PayloadIdentity::Numeric {
            id: u64::from(unsafe { libc::getegid() }),
        };
        let mut init = FileEntry::new(
            "/sbin/init".to_string(),
            ResolvedPayloadNode::from_numeric_source(init_node).unwrap(),
            Some(PayloadContentAuthority {
                sha256: init_hash,
                size: init_content.len() as u64,
            }),
            trove_id,
        );
        init.component_id = Some(component_id);
        init.insert(tx)?;

        let mut provide = ProvideEntry::new(
            trove_id,
            "test-init".to_string(),
            Some("1.0.0".to_string()),
            conary_core::repository::versioning::VersionScheme::Conary,
        );
        provide.insert(tx)?;
        changeset.update_status(tx, ChangesetStatus::Applied)?;

        Ok(())
    })
    .unwrap();
}
