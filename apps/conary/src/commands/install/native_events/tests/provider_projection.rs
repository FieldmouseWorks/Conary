// apps/conary/src/commands/install/native_events/tests/provider_projection.rs

#![cfg(test)]

use super::*;
use conary_core::filesystem::ProjectedNode;
use std::collections::BTreeMap;

fn executable_node() -> ProjectedNode {
    ProjectedNode::Regular { executable: true }
}

fn projected(interpreter: &str, target: ProjectedNode) -> NativeEventPathProjection {
    NativeEventPathProjection::Projected {
        introduced: BTreeMap::from([(interpreter.trim_start_matches('/').to_string(), target)]),
        explicitly_removed: BTreeSet::new(),
    }
}

fn rpm_event(
    bundle: &NativeLifecycleBundle,
    stage: NativeEventStage,
    entry_id: &str,
    placement: NativeEventPlacement,
    args: Vec<String>,
) -> NativeTransactionEvent {
    NativeTransactionEvent {
        owner_package: bundle.source_package.clone(),
        owner_version: bundle.source_version.clone(),
        owner_arch: bundle.source_arch.clone(),
        source_format: "rpm".to_string(),
        stage,
        program: NativeEventProgram::BundleEntry {
            entry_id: entry_id.to_string(),
        },
        args,
        stdin: Vec::new(),
        matched_targets: Vec::new(),
        rpm_trigger_owner: None,
        deb_package_refcount: None,
        order_key: bundle.source_package.clone(),
        placement,
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
fn post_payload_preflight_accepts_incoming_interpreter() {
    let interpreter = "/opt/incoming-runtime/bin/sh";
    let bundle = rpm_bundle_for_phase(
        "incoming-runtime-user",
        "1",
        "rpm:%post",
        LifecyclePath::PostInstall,
        interpreter,
    );
    let event = rpm_event(
        &bundle,
        NativeEventStage::PackagePostInstall,
        "rpm:%post",
        NativeEventPlacement::TransactionElement {
            transaction_index: 0,
        },
        vec!["1".to_string()],
    );
    let prepared =
        prepared_with_projected_event(bundle, event, projected(interpreter, executable_node()));
    let target_root = tempfile::tempdir().unwrap();

    prepared
        .preflight(target_root.path(), &ExecutionMode::Install)
        .expect("post-payload preflight must use the incoming typed projection");
}

#[test]
fn post_payload_preflight_rejects_incoming_interpreter() {
    let interpreter = "/opt/incoming-runtime/bin/sh";
    let bundle = rpm_bundle_for_phase(
        "incoming-runtime-user",
        "1",
        "rpm:%pre",
        LifecyclePath::PreInstall,
        interpreter,
    );
    let event = rpm_event(
        &bundle,
        NativeEventStage::PackagePreInstall,
        "rpm:%pre",
        NativeEventPlacement::TransactionElement {
            transaction_index: 0,
        },
        vec!["1".to_string()],
    );
    let prepared =
        prepared_with_projected_event(bundle, event, NativeEventPathProjection::CurrentRoot);
    let target_root = tempfile::tempdir().unwrap();

    let error = prepared
        .preflight(target_root.path(), &ExecutionMode::Install)
        .expect_err("pre-payload preflight must validate the current root");
    assert_missing_interpreter(&error, interpreter);
}

#[test]
fn absolute_symlink_interpreter_does_not_follow_the_host() {
    let interpreter = "/opt/runtime/bin/sh";
    let bundle = rpm_bundle_for_phase(
        "symlink-runtime-user",
        "1",
        "rpm:%pre",
        LifecyclePath::PreInstall,
        interpreter,
    );
    let event = rpm_event(
        &bundle,
        NativeEventStage::PackagePreInstall,
        "rpm:%pre",
        NativeEventPlacement::TransactionElement {
            transaction_index: 0,
        },
        vec!["1".to_string()],
    );
    let prepared =
        prepared_with_projected_event(bundle, event, NativeEventPathProjection::CurrentRoot);
    let target_root = tempfile::tempdir().unwrap();
    // The absolute target exists on the host, and a host-following `.exists()`
    // would accept it. The selected-root resolver must treat it as root-relative.
    assert!(Path::new("/bin/sh").is_file());
    std::fs::create_dir_all(target_root.path().join("opt/runtime/bin")).unwrap();
    symlink("/bin/sh", target_root.path().join("opt/runtime/bin/sh")).unwrap();

    let error = prepared
        .preflight(target_root.path(), &ExecutionMode::Install)
        .expect_err("an absolute symlink target outside the selected root is unavailable");
    assert_missing_interpreter(&error, interpreter);

    // Control: the same absolute target materialized inside the selected root.
    std::fs::create_dir_all(target_root.path().join("bin")).unwrap();
    std::fs::write(target_root.path().join("bin/sh"), b"#!/bin/sh\n").unwrap();
    let mut permissions = std::fs::metadata(target_root.path().join("bin/sh"))
        .unwrap()
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(target_root.path().join("bin/sh"), permissions).unwrap();

    prepared
        .preflight(target_root.path(), &ExecutionMode::Install)
        .expect("the in-root absolute target must satisfy interpreter preflight");
}

#[test]
fn declared_path_capability_without_a_payload_node_is_unavailable() {
    let (_temp, db_path) = crate::commands::test_helpers::create_test_db();
    let conn = conary_core::db::open(&db_path).unwrap();
    let provider = conary_core::packages::traits::ProvidedCapability {
        kind: conary_core::repository::dependency_model::RepositoryCapabilityKind::Generic,
        name: "/bin/sh".to_string(),
        version: None,
        version_relation: None,
        version_scheme: VersionScheme::Rpm,
        architecture_qualifier:
            conary_core::repository::dependency_model::ProvideArchitectureQualifier::Implicit,
        provenance: conary_core::repository::dependency_model::CapabilityProvenance::AuthorDeclared,
    };
    let consumer_bundle = rpm_bundle_for_phase(
        "crypto-policies-fixture",
        "1",
        "rpm:%pre",
        LifecyclePath::PreInstall,
        "/bin/sh",
    );
    let target_root = tempfile::tempdir().unwrap();

    let prepare = |provider_nodes: BTreeMap<String, ProjectedNode>| {
        PreparedNativeTransaction::prepare_batch(
            &conn,
            &[
                NativeInstallInput {
                    package_name: "bash-fixture",
                    package_version: "1",
                    package_arch: Some("x86_64"),
                    version_scheme: VersionScheme::Rpm,
                    provides: std::slice::from_ref(&provider),
                    new_bundle: None,
                    old_trove: None,
                    relation_removals: &[],
                    relation_deconfigurations: &[],
                    paths: vec!["/bin/sh".to_string()],
                    new_path_nodes: provider_nodes,
                },
                NativeInstallInput {
                    package_name: "crypto-policies-fixture",
                    package_version: "1",
                    package_arch: Some("x86_64"),
                    version_scheme: VersionScheme::Rpm,
                    provides: &[],
                    new_bundle: Some(&consumer_bundle),
                    old_trove: None,
                    relation_removals: &[],
                    relation_deconfigurations: &[],
                    paths: vec!["/etc/crypto-policies/config".to_string()],
                    new_path_nodes: BTreeMap::new(),
                },
            ],
        )
        .unwrap()
    };

    let without_node = prepare(BTreeMap::new());
    let error = without_node
        .preflight(target_root.path(), &ExecutionMode::Install)
        .expect_err("a declared path capability with no payload node is unavailable");
    assert_missing_interpreter(&error, "/bin/sh");

    let with_node = prepare(BTreeMap::from([("/bin/sh".to_string(), executable_node())]));
    with_node
        .preflight(target_root.path(), &ExecutionMode::Install)
        .expect("the payload node backing the capability must satisfy preflight");
}

#[test]
fn post_payload_preflight_resolves_a_hardlinked_interpreter_from_the_payload() {
    let (_temp, db_path) = crate::commands::test_helpers::create_test_db();
    let conn = conary_core::db::open(&db_path).unwrap();
    let provider_nodes = BTreeMap::from([
        (
            "/usr/bin/sh".to_string(),
            ProjectedNode::Hardlink {
                target: "/usr/lib/sh".to_string(),
            },
        ),
        (
            "/usr/lib/sh".to_string(),
            ProjectedNode::Hardlink {
                target: "/usr/lib/sh-real".to_string(),
            },
        ),
        ("/usr/lib/sh-real".to_string(), executable_node()),
    ]);
    let consumer_bundle = rpm_bundle_for_phase(
        "hardlink-consumer",
        "1",
        "rpm:%post",
        LifecyclePath::PostInstall,
        "/usr/bin/sh",
    );
    let prepared = PreparedNativeTransaction::prepare_batch(
        &conn,
        &[
            NativeInstallInput {
                package_name: "hardlink-provider",
                package_version: "1",
                package_arch: Some("x86_64"),
                version_scheme: VersionScheme::Rpm,
                provides: &[],
                new_bundle: None,
                old_trove: None,
                relation_removals: &[],
                relation_deconfigurations: &[],
                paths: vec![
                    "/usr/bin/sh".to_string(),
                    "/usr/lib/sh".to_string(),
                    "/usr/lib/sh-real".to_string(),
                ],
                new_path_nodes: provider_nodes,
            },
            NativeInstallInput {
                package_name: "hardlink-consumer",
                package_version: "1",
                package_arch: Some("x86_64"),
                version_scheme: VersionScheme::Rpm,
                provides: &[],
                new_bundle: Some(&consumer_bundle),
                old_trove: None,
                relation_removals: &[],
                relation_deconfigurations: &[],
                paths: vec!["/etc/hardlink-consumer.conf".to_string()],
                new_path_nodes: BTreeMap::new(),
            },
        ],
    )
    .unwrap();
    let target_root = tempfile::tempdir().unwrap();

    prepared
        .preflight(target_root.path(), &ExecutionMode::Install)
        .expect("a payload hardlink chain must resolve as the projected interpreter");
}

#[test]
fn projected_alias_removal_hides_the_effective_interpreter() {
    let interpreter = "/bin/sh";
    let bundle = rpm_bundle_for_phase(
        "alias-consumer",
        "1",
        "rpm:%post",
        LifecyclePath::PostInstall,
        interpreter,
    );
    let event = rpm_event(
        &bundle,
        NativeEventStage::PackagePostInstall,
        "rpm:%post",
        NativeEventPlacement::TransactionElement {
            transaction_index: 0,
        },
        vec!["1".to_string()],
    );
    let target_root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(target_root.path().join("usr/bin")).unwrap();
    symlink("usr/bin", target_root.path().join("bin")).unwrap();
    std::fs::write(target_root.path().join("usr/bin/sh"), b"#!/bin/sh\n").unwrap();
    let mut permissions = std::fs::metadata(target_root.path().join("usr/bin/sh"))
        .unwrap()
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(target_root.path().join("usr/bin/sh"), permissions).unwrap();

    let removed = NativeEventPathProjection::Projected {
        introduced: BTreeMap::new(),
        explicitly_removed: BTreeSet::from(["bin/sh".to_string()]),
    };
    let prepared = prepared_with_projected_event(bundle.clone(), event.clone(), removed);
    let error = prepared
        .preflight(target_root.path(), &ExecutionMode::Install)
        .expect_err("removing the alias spelling must hide the effective interpreter");
    assert_missing_interpreter(&error, interpreter);

    // Control: with nothing removed, the alias resolves the on-disk file.
    let prepared = prepared_with_projected_event(
        bundle,
        event,
        NativeEventPathProjection::Projected {
            introduced: BTreeMap::new(),
            explicitly_removed: BTreeSet::new(),
        },
    );
    prepared
        .preflight(target_root.path(), &ExecutionMode::Install)
        .expect("the alias target remains available when nothing is removed");
}
