// apps/conary/src/commands/install/native_events/tests/payload_effects_projection.rs

#![cfg(test)]

//! Native event-time interpreter projection over the full payload effects plan.
//!
//! These mirror the CCS divergence tests for a native element: (b) a locally
//! modified config primary kept in favour of a suffixed incoming copy, and (g)
//! a preserved selected-root directory alias the interpreter resolves through.

use super::*;
use crate::commands::install::payload_effects::{
    ElementPayloadEffectInput, PayloadEffectFiles, ProjectedPayloadEffects,
    plan_element_payload_projection,
};
use crate::commands::install::payload_identity::PlanIdentityMode;
use crate::commands::install::{InstallSemantics, PackageFormatType};
use conary_core::db::models::{ConfigFile, ConfigSource};
use conary_core::packages::config_authority::{ConfigPayloadAssociation, SourceConfigDeclaration};
use conary_core::packages::deb::authority::DebianConfigDeclaration;
use conary_core::packages::payload::{PackagePayloadFile, ReopenablePayload};
use conary_core::payload::{
    PayloadContentAuthority, PayloadIdentity, PayloadNode, PayloadNodeKind, PayloadTimestamp,
};
use std::os::unix::fs::PermissionsExt;

const CONFIG_INTERPRETER: &str = "/etc/hook-interpreter";
const ALIAS_INTERPRETER: &str = "/bin/sh";

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

fn directory_payload(path: &str, mode: u32) -> PackagePayloadFile {
    PackagePayloadFile::new(
        path.to_string(),
        numeric_node(PayloadNodeKind::Directory, libc::S_IFDIR | (mode & 0o7777)),
        None,
        None,
    )
    .unwrap()
}

fn regular_payload(path: &str, bytes: &[u8], mode: u32) -> PackagePayloadFile {
    let node = numeric_node(
        PayloadNodeKind::Regular {
            hardlink_identity: None,
        },
        libc::S_IFREG | (mode & 0o7777),
    );
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

fn write_file(root: &Path, package_path: &str, bytes: &[u8], mode: u32) {
    let path = root.join(package_path.trim_start_matches('/'));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, bytes).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn deb_post_install_bundle(
    package_name: &str,
    version: &str,
    interpreter: &str,
) -> NativeLifecycleBundle {
    let mut bundle = deb_remove_with_recovery_bundle(package_name, version);
    bundle.entries = vec![deb_entry(
        "deb:postinst",
        LifecyclePath::PostInstall,
        DebControlMember::Postinst,
        DebMaintainerMode::Configure,
        interpreter,
    )];
    bundle
}

/// The installed lifecycle contract for an upgraded Debian package. The
/// refcount projection needs the old package instance even when its own bundle
/// declares no maintainer entries.
fn installed_deb_bundle(package_name: &str, version: &str) -> NativeLifecycleBundle {
    let mut bundle = deb_remove_with_recovery_bundle(package_name, version);
    bundle.entries.clear();
    bundle
}

fn plan_effects(
    conn: &rusqlite::Connection,
    root: &Path,
    semantics: InstallSemantics,
    package_name: &str,
    replacing_trove_id: Option<i64>,
    declarations: &[SourceConfigDeclaration],
    files: &[PackagePayloadFile],
) -> ProjectedPayloadEffects {
    plan_element_payload_projection(
        conn,
        root,
        ElementPayloadEffectInput {
            semantics,
            package_name,
            relation_removals: &[],
            replacing_trove_id,
            config_declarations: declarations,
            files: PayloadEffectFiles::Extracted(files),
            identity_mode: PlanIdentityMode::EventProjection,
        },
    )
    .unwrap()
}

fn native_input<'a>(
    effects: &ProjectedPayloadEffects,
    bundle: &'a NativeLifecycleBundle,
    old_trove: Option<&'a Trove>,
    declared_paths: &[&str],
    version_scheme: VersionScheme,
) -> NativeInstallInput<'a> {
    NativeInstallInput {
        package_name: &bundle.source_package,
        package_version: &bundle.source_version,
        package_arch: bundle.source_arch.as_deref(),
        version_scheme,
        provides: &[],
        new_bundle: Some(bundle),
        old_trove,
        relation_removals: &[],
        relation_deconfigurations: &[],
        paths: declared_paths.iter().map(|path| path.to_string()).collect(),
        new_path_nodes: effects.projected_nodes(),
    }
}

fn assert_missing_interpreter(error: &anyhow::Error, interpreter: &str) {
    assert!(
        matches!(
            error.downcast_ref::<conary_core::scriptlet::NativeLifecyclePreflightError>(),
            Some(conary_core::scriptlet::NativeLifecyclePreflightError::MissingInterpreter {
                interpreter: actual,
                ..
            }) if actual == interpreter
        ),
        "unexpected error: {error:#}"
    );
}

#[test]
fn kept_config_primary_is_refused_and_a_pristine_primary_admits() {
    let (_temp, db_path) = crate::commands::test_helpers::create_test_db();
    let conn = conary_core::db::open(&db_path).unwrap();
    let root = tempfile::tempdir().unwrap();

    let mut old = Trove::new(
        "config-hook-fixture".to_string(),
        "1.0.0".to_string(),
        TroveType::Package,
        VersionScheme::Debian,
    );
    old.debian_multi_arch = Some(DebianMultiArch::No);
    let old_trove_id = old.insert(&conn).unwrap();
    let old = Trove::find_by_id(&conn, old_trove_id).unwrap().unwrap();
    InstalledNativeLifecycleBundle::new(
        old_trove_id,
        None,
        &installed_deb_bundle("config-hook-fixture", "1.0.0"),
    )
    .unwrap()
    .insert_or_replace(&conn)
    .unwrap();
    let mut config = ConfigFile::new(
        CONFIG_INTERPRETER.to_string(),
        old_trove_id,
        conary_core::hash::sha256(b"old"),
    );
    config.source = ConfigSource::Deb;
    config.original_md5 = crate::commands::install::config_files::debian_original_md5(
        ConfigSource::Deb,
        true,
        b"old",
    );
    config.insert(&conn).unwrap();

    let declarations = vec![SourceConfigDeclaration::Debian(DebianConfigDeclaration {
        control_index: 0,
        path: CONFIG_INTERPRETER.to_string(),
        remove_on_upgrade: false,
        payload: ConfigPayloadAssociation::Matched,
    })];
    let semantics = InstallSemantics::native_package(PackageFormatType::Deb);
    let bundle = deb_post_install_bundle("config-hook-fixture", "2.0.0", CONFIG_INTERPRETER);
    let incoming = [regular_payload(CONFIG_INTERPRETER, b"new", 0o755)];
    let declared_paths = [CONFIG_INTERPRETER];

    // The locally modified primary is kept; the incoming executable lands at
    // the suffixed path, so the primary stays non-executable.
    write_file(root.path(), CONFIG_INTERPRETER, b"local", 0o644);
    let modified = plan_effects(
        &conn,
        root.path(),
        semantics,
        "config-hook-fixture",
        Some(old_trove_id),
        &declarations,
        &incoming,
    );
    assert_eq!(modified.projected_nodes().len(), 1);
    assert!(!modified.projected_nodes().contains_key(CONFIG_INTERPRETER));
    let refused = PreparedNativeTransaction::prepare_install(
        &conn,
        native_input(
            &modified,
            &bundle,
            Some(&old),
            &declared_paths,
            VersionScheme::Debian,
        ),
    )
    .unwrap();
    let error = refused
        .preflight(root.path(), &ExecutionMode::Install)
        .expect_err("a kept non-executable config primary must not admit its interpreter");
    assert_missing_interpreter(&error, CONFIG_INTERPRETER);

    // Control through the same fixture: a pristine primary is replaced by the
    // executable incoming copy and the interpreter resolves.
    write_file(root.path(), CONFIG_INTERPRETER, b"old", 0o644);
    let pristine = plan_effects(
        &conn,
        root.path(),
        semantics,
        "config-hook-fixture",
        Some(old_trove_id),
        &declarations,
        &incoming,
    );
    assert_eq!(pristine.projected_nodes().len(), 1);
    assert!(pristine.projected_nodes().contains_key(CONFIG_INTERPRETER));
    let admitted = PreparedNativeTransaction::prepare_install(
        &conn,
        native_input(
            &pristine,
            &bundle,
            Some(&old),
            &declared_paths,
            VersionScheme::Debian,
        ),
    )
    .unwrap();
    admitted
        .preflight(root.path(), &ExecutionMode::Install)
        .expect("a pristine primary is replaced by the executable incoming copy");
}

#[test]
fn preserved_directory_alias_projects_the_shipped_interpreter() {
    let (_temp, db_path) = crate::commands::test_helpers::create_test_db();
    let conn = conary_core::db::open(&db_path).unwrap();
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("usr/bin")).unwrap();
    std::os::unix::fs::symlink("usr/bin", root.path().join("bin")).unwrap();

    let semantics = InstallSemantics::native_package(PackageFormatType::Rpm);
    let bundle = rpm_bundle_for_phase(
        "alias-hook-fixture",
        "1.0.0",
        "rpm:%post",
        LifecyclePath::PostInstall,
        ALIAS_INTERPRETER,
    );

    // Admit: the payload ships the effective /usr/bin/sh beneath the preserved
    // /bin -> usr/bin alias, so execution materializes /bin/sh.
    let shipped = [
        directory_payload("/bin", 0o755),
        regular_payload("/usr/bin/sh", b"#!/bin/sh\n", 0o755),
    ];
    let effects = plan_effects(
        &conn,
        root.path(),
        semantics,
        "alias-hook-fixture",
        None,
        &[],
        &shipped,
    );
    let admitted = PreparedNativeTransaction::prepare_install(
        &conn,
        native_input(
            &effects,
            &bundle,
            None,
            &["/bin", "/usr/bin/sh"],
            VersionScheme::Rpm,
        ),
    )
    .unwrap();
    admitted
        .preflight(root.path(), &ExecutionMode::Install)
        .expect("the preserved alias must reach the shipped interpreter");

    // Refuse: without the shipped target the alias names no interpreter.
    let directory_only = [directory_payload("/bin", 0o755)];
    let effects = plan_effects(
        &conn,
        root.path(),
        semantics,
        "alias-hook-fixture",
        None,
        &[],
        &directory_only,
    );
    let refused = PreparedNativeTransaction::prepare_install(
        &conn,
        native_input(&effects, &bundle, None, &["/bin"], VersionScheme::Rpm),
    )
    .unwrap();
    let error = refused
        .preflight(root.path(), &ExecutionMode::Install)
        .expect_err("without the shipped target the alias is unavailable");
    assert_missing_interpreter(&error, ALIAS_INTERPRETER);
}
