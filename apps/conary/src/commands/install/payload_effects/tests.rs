// apps/conary/src/commands/install/payload_effects/tests.rs

#![cfg(test)]

use super::*;
use crate::commands::install::inner;
use crate::commands::install::payload_identity::{IdentityKind, PlanIdentityMode};
use crate::commands::install::shared_directory::DirectoryPathPlan;
use crate::commands::install::{InstallSemantics, PackageFormatType};
use conary_core::config_transaction::{ConfigInstallDecision, ConfigSuffix};
use conary_core::db::models::{ConfigFile, ConfigSource, FileEntry, Trove, TroveType};
use conary_core::filesystem::{CasStore, ProjectedNode};
use conary_core::packages::config_authority::ConfigPayloadAssociation;
use conary_core::packages::payload::{PackagePayloadFile, ReopenablePayload};
use conary_core::packages::traits::PackageFile;
use conary_core::packages::{PackageFormat, PackagePayload};
use conary_core::payload::{
    PayloadContentAuthority, PayloadIdentity, PayloadNode, PayloadNodeKind, PayloadSharingPolicy,
    PayloadTimestamp, ResolvedPayloadNode,
};
use conary_core::repository::dependency_model::RepositoryRequirementGroup;
use conary_core::repository::versioning::VersionScheme;
use std::path::PathBuf;

struct Fixture {
    _temp: tempfile::TempDir,
    conn: rusqlite::Connection,
    root: PathBuf,
    cas: CasStore,
}

fn fixture() -> Fixture {
    let (temp, db_path) = crate::commands::test_helpers::create_test_db();
    let conn = conary_core::db::open(db_path).unwrap();
    let root = temp.path().join("selected");
    std::fs::create_dir_all(&root).unwrap();
    let cas = CasStore::new(temp.path().join("objects")).unwrap();
    Fixture {
        _temp: temp,
        conn,
        root,
        cas,
    }
}

fn insert_trove(conn: &rusqlite::Connection, name: &str) -> i64 {
    Trove::new(
        name.to_string(),
        "1.0.0".to_string(),
        TroveType::Package,
        VersionScheme::Conary,
    )
    .insert(conn)
    .unwrap()
}

fn numeric_node(kind: PayloadNodeKind, mode: u32) -> PayloadNode {
    PayloadNode {
        kind,
        mode,
        user: PayloadIdentity::Numeric { id: 0 },
        group: PayloadIdentity::Numeric { id: 0 },
        mtime: PayloadTimestamp::UNIX_EPOCH,
        xattrs: Default::default(),
    }
}

fn regular_node(mode: u32) -> PayloadNode {
    numeric_node(
        PayloadNodeKind::Regular {
            hardlink_identity: None,
        },
        libc::S_IFREG | (mode & 0o7777),
    )
}

fn directory_node(mode: u32) -> PayloadNode {
    numeric_node(PayloadNodeKind::Directory, libc::S_IFDIR | (mode & 0o7777))
}

fn hardlink_node(target: &str, identity: &str, mode: u32) -> PayloadNode {
    numeric_node(
        PayloadNodeKind::Hardlink {
            target: target.to_string(),
            identity: identity.to_string(),
        },
        libc::S_IFREG | (mode & 0o7777),
    )
}

fn regular_payload(path: &str, bytes: &[u8], mode: u32) -> PackagePayloadFile {
    regular_payload_with_node(path, bytes, regular_node(mode))
}

/// A regular payload with exact content authority and a caller-chosen node,
/// for example one owned by a named user.
fn regular_payload_with_node(path: &str, bytes: &[u8], node: PayloadNode) -> PackagePayloadFile {
    let authority = PayloadContentAuthority {
        sha256: conary_core::hash::sha256(bytes),
        size: bytes.len() as u64,
    };
    PackagePayloadFile::new(
        path.to_string(),
        node,
        Some(authority),
        Some(ReopenablePayload::from_in_memory_bytes(bytes.to_vec())),
    )
    .unwrap()
}

fn node_payload(path: &str, node: PayloadNode) -> PackagePayloadFile {
    PackagePayloadFile::new(path.to_string(), node, None, None).unwrap()
}

fn directory_payload(path: &str, mode: u32) -> PackagePayloadFile {
    node_payload(path, directory_node(mode))
}

fn hardlink_payload(path: &str, target: &str, identity: &str, mode: u32) -> PackagePayloadFile {
    node_payload(path, hardlink_node(target, identity, mode))
}

fn alpm_matched(path: &str) -> SourceConfigDeclaration {
    SourceConfigDeclaration::Alpm(
        conary_core::packages::arch::authority::AlpmConfigDeclaration {
            pkginfo_index: 0,
            source_path: path.trim_start_matches('/').to_string(),
            path: path.to_string(),
            installed_hash: None,
            payload: ConfigPayloadAssociation::Matched,
        },
    )
}

/// Build the plan from the extraction form and from the stored form and prove
/// the typed effects are identical.
fn assert_forms_agree(
    fixture: &Fixture,
    semantics: InstallSemantics,
    declarations: &[SourceConfigDeclaration],
    extracted: &[PackagePayloadFile],
) {
    let extracted_plan = plan_extracted_for(fixture, semantics, declarations, extracted);
    let stored = inner::store_extracted_files_in_cas(&fixture.cas, extracted).unwrap();
    let stored_plan = plan_element_payload_effects(
        &fixture.conn,
        &fixture.root,
        ElementPayloadEffectInput {
            semantics,
            package_name: "fixture",
            relation_removals: &[],
            replacing_trove_id: None,
            config_declarations: declarations,
            files: PayloadEffectFiles::Stored {
                cas: &fixture.cas,
                files: &stored,
            },
            identity_mode: PlanIdentityMode::Authoritative,
        },
    )
    .unwrap();

    assert_eq!(
        extracted_plan, stored_plan,
        "the pre-CAS and post-CAS plans must be the same typed effect"
    );
}

fn plan_extracted_for(
    fixture: &Fixture,
    semantics: InstallSemantics,
    declarations: &[SourceConfigDeclaration],
    extracted: &[PackagePayloadFile],
) -> ElementPayloadEffects {
    plan_element_payload_effects(
        &fixture.conn,
        &fixture.root,
        ElementPayloadEffectInput {
            semantics,
            package_name: "fixture",
            relation_removals: &[],
            replacing_trove_id: None,
            config_declarations: declarations,
            files: PayloadEffectFiles::Extracted(extracted),
            identity_mode: PlanIdentityMode::Authoritative,
        },
    )
    .unwrap()
}

#[test]
fn usr_merge_alias_plan_agrees_before_and_after_cas() {
    let fixture = fixture();
    std::fs::create_dir_all(fixture.root.join("usr/bin")).unwrap();
    std::os::unix::fs::symlink("usr/bin", fixture.root.join("bin")).unwrap();

    let extracted = vec![
        directory_payload("/bin", 0o755),
        regular_payload("/bin/tool", b"tool", 0o755),
    ];
    let semantics = InstallSemantics::native_package(PackageFormatType::Rpm);
    let plan = plan_extracted_for(&fixture, semantics, &[], &extracted);

    assert!(
        matches!(
            plan.directory_plan.path("/bin"),
            Some(DirectoryPathPlan::ApplyThroughSymlink { .. })
        ),
        "the incoming /bin directory must follow the usr-merge alias"
    );
    assert_eq!(plan.through_symlink_files.len(), 1);
    assert_eq!(plan.through_symlink_files[0].path, "/usr/bin");

    assert_forms_agree(&fixture, semantics, &[], &extracted);
}

#[test]
fn modified_config_alternative_plan_agrees_before_and_after_cas() {
    let fixture = fixture();
    std::fs::create_dir_all(fixture.root.join("etc")).unwrap();
    std::fs::write(fixture.root.join("etc/demo.conf"), b"local").unwrap();
    let old_trove = insert_trove(&fixture.conn, "old-owner");
    let mut old = ConfigFile::new(
        "/etc/demo.conf".to_string(),
        old_trove,
        conary_core::hash::sha256(b"old"),
    );
    old.source = ConfigSource::Arch;
    old.insert(&fixture.conn).unwrap();

    let declarations = vec![alpm_matched("/etc/demo.conf")];
    let extracted = vec![regular_payload("/etc/demo.conf", b"new", 0o100644)];
    let semantics = InstallSemantics::native_package(PackageFormatType::Arch);
    let plan = plan_extracted_for(&fixture, semantics, &declarations, &extracted);

    assert_eq!(plan.install_files.len(), 1);
    assert_eq!(plan.install_files[0].path, "/etc/demo.conf.pacnew");
    assert_eq!(
        plan.config_decisions,
        vec![ConfigInstallDecisionRecord {
            path: "/etc/demo.conf".to_string(),
            decision: ConfigInstallDecision::InstallAlternative(ConfigSuffix::PacNew),
        }]
    );

    assert_forms_agree(&fixture, semantics, &declarations, &extracted);
}

#[test]
fn preflight_entry_point_matches_the_generic_planner() {
    let fixture = fixture();
    std::fs::create_dir_all(fixture.root.join("etc")).unwrap();
    std::fs::write(fixture.root.join("etc/demo.conf"), b"local").unwrap();
    let old_trove = insert_trove(&fixture.conn, "old-owner");
    let mut old = ConfigFile::new(
        "/etc/demo.conf".to_string(),
        old_trove,
        conary_core::hash::sha256(b"old"),
    );
    old.source = ConfigSource::Arch;
    old.insert(&fixture.conn).unwrap();

    let declarations = vec![alpm_matched("/etc/demo.conf")];
    let extracted = vec![regular_payload("/etc/demo.conf", b"new", 0o100644)];
    let semantics = InstallSemantics::native_package(PackageFormatType::Arch);
    let direct = plan_extracted_for(&fixture, semantics, &declarations, &extracted);

    let pkg = FakePackage {
        declarations: declarations.clone(),
    };
    let via_entry = plan_extracted_element_payload_effects(
        &fixture.conn,
        &fixture.root,
        &pkg,
        &extracted,
        semantics,
        None,
        &[],
    )
    .unwrap();

    assert_eq!(direct, via_entry);
}

/// A named owner a pre-payload lifecycle event creates must not be required
/// during the event-time projection, while the authoritative planner still
/// refuses it.
#[test]
fn event_projection_defers_a_name_the_root_does_not_define() {
    let fixture = fixture();
    std::fs::create_dir_all(fixture.root.join("etc")).unwrap();
    std::fs::write(
        fixture.root.join("etc/passwd"),
        "root:x:0:0:root:/root:/bin/sh\n",
    )
    .unwrap();
    std::fs::write(fixture.root.join("etc/group"), "root:x:0:\n").unwrap();

    let mut node = regular_node(0o755);
    node.user = PayloadIdentity::Named {
        name: "summary-late-user".to_string(),
    };
    node.group = PayloadIdentity::Named {
        name: "summary-late-group".to_string(),
    };
    let extracted = vec![regular_payload_with_node("/usr/bin/late", b"late\n", node)];
    let semantics = InstallSemantics::native_package(PackageFormatType::Rpm);

    // Positive control through the same fixture: the event-time projection
    // admits the payload and records the typed pending owners.
    let projected = plan_element_payload_projection(
        &fixture.conn,
        &fixture.root,
        ElementPayloadEffectInput {
            semantics,
            package_name: "late-owner",
            relation_removals: &[],
            replacing_trove_id: None,
            config_declarations: &[],
            files: PayloadEffectFiles::Extracted(&extracted),
            identity_mode: PlanIdentityMode::EventProjection,
        },
    )
    .unwrap();
    assert_eq!(
        projected.pending_owners().cloned().collect::<Vec<_>>(),
        vec![
            PendingOwner {
                kind: IdentityKind::User,
                name: "summary-late-user".to_string(),
            },
            PendingOwner {
                kind: IdentityKind::Group,
                name: "summary-late-group".to_string(),
            },
        ]
    );
    assert_eq!(
        projected.projected_nodes().get("/usr/bin/late"),
        Some(&ProjectedNode::Regular { executable: true })
    );

    // The same input under Authoritative planning keeps the typed missing-name
    // refusal.
    let error = plan_element_payload_effects(
        &fixture.conn,
        &fixture.root,
        ElementPayloadEffectInput {
            semantics,
            package_name: "late-owner",
            relation_removals: &[],
            replacing_trove_id: None,
            config_declarations: &[],
            files: PayloadEffectFiles::Extracted(&extracted),
            identity_mode: PlanIdentityMode::Authoritative,
        },
    )
    .unwrap_err();
    match error.downcast_ref::<crate::commands::install::payload_identity::PayloadIdentityError>() {
        Some(crate::commands::install::payload_identity::PayloadIdentityError::MissingNames {
            kind,
            names,
            ..
        }) => {
            assert_eq!(*kind, IdentityKind::User);
            assert_eq!(names, "summary-late-user");
        }
        other => panic!("expected the typed missing-name refusal, got {other:?}"),
    }
}

/// A pending owner whose path already has database authority resolves on disk
/// rather than being overlaid, which is the projection's refusing direction.
#[test]
fn event_projection_resolves_a_pending_owner_path_with_existing_authority_on_disk() {
    let fixture = fixture();
    std::fs::create_dir_all(fixture.root.join("etc")).unwrap();
    std::fs::write(
        fixture.root.join("etc/passwd"),
        "root:x:0:0:root:/root:/bin/sh\n",
    )
    .unwrap();
    std::fs::write(fixture.root.join("etc/group"), "root:x:0:\n").unwrap();

    let owner = insert_trove(&fixture.conn, "existing-owner");
    let mut entry = FileEntry::new(
        "/usr/bin/late".to_string(),
        ResolvedPayloadNode::from_numeric_source(regular_node(0o755)).unwrap(),
        Some(PayloadContentAuthority {
            sha256: conary_core::hash::sha256(b"old"),
            size: 3,
        }),
        owner,
    );
    entry.insert(&fixture.conn).unwrap();

    let mut node = regular_node(0o755);
    node.user = PayloadIdentity::Named {
        name: "summary-late-user".to_string(),
    };
    node.group = PayloadIdentity::Named {
        name: "summary-late-group".to_string(),
    };
    let extracted = vec![regular_payload_with_node("/usr/bin/late", b"late\n", node)];
    let semantics = InstallSemantics::native_package(PackageFormatType::Rpm);

    let projected = plan_element_payload_projection(
        &fixture.conn,
        &fixture.root,
        ElementPayloadEffectInput {
            semantics,
            package_name: "late-owner",
            relation_removals: &[],
            replacing_trove_id: None,
            config_declarations: &[],
            files: PayloadEffectFiles::Extracted(&extracted),
            identity_mode: PlanIdentityMode::EventProjection,
        },
    )
    .unwrap();

    assert!(!projected.projected_nodes().contains_key("/usr/bin/late"));
    assert!(
        projected
            .pending_owners()
            .any(|owner| owner.name == "summary-late-user")
    );
}

/// The single-install path: preflight derives effects from the extraction form
/// while `apply_payload` derives them from the stored CAS form. Both must be the
/// same typed effect.
#[test]
fn preflight_extraction_plan_matches_the_stored_apply_plan() {
    let fixture = fixture();
    std::fs::create_dir_all(fixture.root.join("etc")).unwrap();
    std::fs::write(fixture.root.join("etc/demo.conf"), b"local").unwrap();
    let old_trove = insert_trove(&fixture.conn, "old-owner");
    let mut old = ConfigFile::new(
        "/etc/demo.conf".to_string(),
        old_trove,
        conary_core::hash::sha256(b"old"),
    );
    old.source = ConfigSource::Arch;
    old.insert(&fixture.conn).unwrap();

    let declarations = vec![alpm_matched("/etc/demo.conf")];
    let extracted = vec![regular_payload("/etc/demo.conf", b"new", 0o100644)];
    let semantics = InstallSemantics::native_package(PackageFormatType::Arch);
    let pkg = FakePackage {
        declarations: declarations.clone(),
    };
    let replacing = Trove::find_by_id(&fixture.conn, old_trove).unwrap();

    let preflight = plan_extracted_element_payload_effects(
        &fixture.conn,
        &fixture.root,
        &pkg,
        &extracted,
        semantics,
        replacing.as_ref(),
        &[],
    )
    .unwrap();
    let stored = inner::store_extracted_files_in_cas(&fixture.cas, &extracted).unwrap();
    let apply = plan_element_payload_effects(
        &fixture.conn,
        &fixture.root,
        ElementPayloadEffectInput {
            semantics,
            package_name: pkg.name(),
            relation_removals: &[],
            replacing_trove_id: Some(old_trove),
            config_declarations: &declarations,
            files: PayloadEffectFiles::Stored {
                cas: &fixture.cas,
                files: &stored,
            },
            identity_mode: PlanIdentityMode::Authoritative,
        },
    )
    .unwrap();

    assert_eq!(
        preflight, apply,
        "preflight's extraction plan must equal the stored plan execution applies"
    );
}

#[test]
fn preserved_hardlink_chain_plan_agrees_before_and_after_cas() {
    let fixture = fixture();
    let target = "/usr/share/anchor";
    let content = b"shared";
    let authority = PayloadContentAuthority {
        sha256: conary_core::hash::sha256(content),
        size: content.len() as u64,
    };
    std::fs::create_dir_all(fixture.root.join("usr/share")).unwrap();
    std::fs::write(fixture.root.join(target.trim_start_matches('/')), content).unwrap();
    let anchor_owner = insert_trove(&fixture.conn, "anchor-owner");
    let mut anchor = FileEntry::new(
        target.to_string(),
        ResolvedPayloadNode::from_numeric_source(regular_node(0o644)).unwrap(),
        Some(authority),
        anchor_owner,
    )
    .with_claim_policy(PayloadSharingPolicy::Rpm);
    anchor.insert(&fixture.conn).unwrap();

    let extracted = vec![
        regular_payload(target, content, 0o644),
        hardlink_payload("/usr/share/edge1", target, "chain:1", 0o644),
        hardlink_payload("/usr/share/edge2", "/usr/share/edge1", "chain:1", 0o644),
    ];
    let semantics = InstallSemantics::native_package(PackageFormatType::Rpm);
    let plan = plan_extracted_for(&fixture, semantics, &[], &extracted);

    assert!(plan.directory_plan.preserves_leaf(target));
    assert_eq!(plan.install_files.len(), 2);
    assert_eq!(plan.hardlink_references.len(), 1);
    assert_eq!(plan.hardlink_references[0].path, target);
    assert_eq!(
        plan.install_files[0].node.source.kind,
        PayloadNodeKind::Hardlink {
            target: target.to_string(),
            identity: format!("path:{target}"),
        }
    );

    assert_forms_agree(&fixture, semantics, &[], &extracted);
}

/// A representative batch element carrying every payload kind execution must
/// apply: a directory, a regular file, a declared config, and a preserved
/// hardlink. Batch `apply_payload` consumes the stored-form plan; batch
/// preflight consumes the extraction-form plan. Both must be the same typed
/// effect, or the one planner is not the authority.
#[test]
fn representative_batch_element_plan_agrees_before_and_after_cas() {
    let fixture = fixture();
    std::fs::create_dir_all(fixture.root.join("etc")).unwrap();
    std::fs::create_dir_all(fixture.root.join("usr/share")).unwrap();

    let target = "/usr/share/anchor";
    let content = b"shared";
    let authority = PayloadContentAuthority {
        sha256: conary_core::hash::sha256(content),
        size: content.len() as u64,
    };
    std::fs::write(fixture.root.join(target.trim_start_matches('/')), content).unwrap();
    let anchor_owner = insert_trove(&fixture.conn, "anchor-owner");
    let mut anchor = FileEntry::new(
        target.to_string(),
        ResolvedPayloadNode::from_numeric_source(regular_node(0o644)).unwrap(),
        Some(authority),
        anchor_owner,
    )
    .with_claim_policy(PayloadSharingPolicy::Rpm);
    anchor.insert(&fixture.conn).unwrap();

    let declarations = vec![SourceConfigDeclaration::Rpm(
        conary_core::packages::rpm::authority::RpmConfigDeclaration {
            header_index: 0,
            path: "/etc/batch-demo.conf".to_string(),
            noreplace: false,
            ghost: false,
            missing_ok: false,
            payload: ConfigPayloadAssociation::Matched,
        },
    )];
    let extracted = vec![
        directory_payload("/opt/demo", 0o755),
        regular_payload("/opt/demo/tool", b"tool", 0o755),
        regular_payload("/etc/batch-demo.conf", b"managed=true\n", 0o644),
        regular_payload(target, content, 0o644),
        hardlink_payload("/usr/share/edge", target, "chain:1", 0o644),
    ];
    let semantics = InstallSemantics::native_package(PackageFormatType::Rpm);
    let plan = plan_extracted_for(&fixture, semantics, &declarations, &extracted);

    assert!(
        plan.directory_plan.path("/opt/demo").is_some(),
        "the batch element must exercise a directory payload"
    );
    assert!(
        plan.install_files
            .iter()
            .any(|file| file.path == "/opt/demo/tool"),
        "the batch element must exercise a regular payload"
    );
    assert_eq!(
        plan.config_decisions
            .iter()
            .map(|record| record.path.as_str())
            .collect::<Vec<_>>(),
        vec!["/etc/batch-demo.conf"],
        "the batch element must exercise a declared config"
    );
    assert!(
        plan.hardlink_references
            .iter()
            .any(|file| file.path == target),
        "the batch element must exercise a preserved hardlink reference"
    );

    assert_forms_agree(&fixture, semantics, &declarations, &extracted);
}

#[test]
fn preserved_directory_alias_plan_agrees_before_and_after_cas() {
    let fixture = fixture();
    std::fs::create_dir_all(fixture.root.join("real")).unwrap();
    std::os::unix::fs::symlink("/real", fixture.root.join("shared")).unwrap();

    let extracted = vec![directory_payload("/shared", 0o755)];
    let semantics = InstallSemantics::native_package(PackageFormatType::Deb);
    let plan = plan_extracted_for(&fixture, semantics, &[], &extracted);

    assert!(
        matches!(
            plan.directory_plan.path("/shared"),
            Some(DirectoryPathPlan::PreserveLeaf { .. })
        ),
        "a Debian directory payload must preserve the existing directory alias"
    );
    assert!(plan.install_files.is_empty());

    assert_forms_agree(&fixture, semantics, &[], &extracted);
}

struct FakePackage {
    declarations: Vec<SourceConfigDeclaration>,
}

impl PackageFormat for FakePackage {
    fn parse(_path: &str) -> conary_core::Result<Self> {
        unreachable!("test constructs the package directly")
    }

    fn name(&self) -> &str {
        "payload-effects-fixture"
    }

    fn version(&self) -> &str {
        "1.0.0"
    }

    fn version_scheme(&self) -> VersionScheme {
        VersionScheme::Conary
    }

    fn architecture(&self) -> Option<&str> {
        Some("x86_64")
    }

    fn description(&self) -> Option<&str> {
        None
    }

    fn files(&self) -> &[PackageFile] {
        &[]
    }

    fn requirements(&self) -> &[RepositoryRequirementGroup] {
        &[]
    }

    fn package_payload(&self) -> conary_core::Result<PackagePayload> {
        Ok(PackagePayload::default())
    }

    fn config_declarations(&self) -> conary_core::Result<Vec<SourceConfigDeclaration>> {
        Ok(self.declarations.clone())
    }

    fn to_trove(&self) -> Trove {
        Trove::new(
            self.name().to_string(),
            self.version().to_string(),
            TroveType::Package,
            VersionScheme::Conary,
        )
    }
}
