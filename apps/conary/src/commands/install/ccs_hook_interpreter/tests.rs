// apps/conary/src/commands/install/ccs_hook_interpreter/tests.rs

#![cfg(test)]

use super::*;
use crate::commands::install::payload_effects::{
    ElementPayloadEffectInput, ElementPayloadEffects, PayloadEffectFiles,
    plan_element_payload_effects, projected_node,
};
use crate::commands::install::{InstallSemantics, PackageFormatType};
use crate::commands::{LiveRootContent, LiveRootFile};
use conary_core::db::models::{
    ConfigFile, ConfigSource, ExistingDirectoryMaterialization, FileEntry, Trove, TroveType,
};
use conary_core::filesystem::ProjectedNode;
use conary_core::packages::config_authority::{ConfigPayloadAssociation, SourceConfigDeclaration};
use conary_core::packages::deb::authority::DebianConfigDeclaration;
use conary_core::packages::payload::{PackagePayloadFile, ReopenablePayload};
use conary_core::payload::{
    PayloadContentAuthority, PayloadIdentity, PayloadNode, PayloadNodeKind, PayloadSharingPolicy,
    PayloadTimestamp, ResolvedPayloadNode,
};
use conary_core::repository::versioning::VersionScheme;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const PRESENT: &str = "/usr/bin/hook-interpreter";

/// Unwrap the typed refusal; any other error fails the test.
fn typed(error: anyhow::Error) -> CcsHookInterpreterUnavailable {
    error
        .downcast()
        .expect("refusal must be the typed CcsHookInterpreterUnavailable")
}

fn post_install(interpreter: &str) -> HookInterpreter {
    HookInterpreter {
        phase: HookPhase::PostInstall,
        interpreter: interpreter.to_string(),
    }
}

fn pre_remove(interpreter: &str) -> HookInterpreter {
    HookInterpreter {
        phase: HookPhase::PreRemove,
        interpreter: interpreter.to_string(),
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    conn: rusqlite::Connection,
    root: PathBuf,
}

fn fixture() -> Fixture {
    let (temp, db_path) = crate::commands::test_helpers::create_test_db();
    let conn = conary_core::db::open(&db_path).unwrap();
    let root = temp.path().join("selected");
    fs::create_dir_all(&root).unwrap();
    Fixture {
        _temp: temp,
        conn,
        root,
    }
}

fn rpm_semantics() -> InstallSemantics {
    InstallSemantics::native_package(PackageFormatType::Rpm)
}

fn deb_semantics() -> InstallSemantics {
    InstallSemantics::native_package(PackageFormatType::Deb)
}

fn write_regular(root: &Path, package_path: &str, bytes: &[u8], mode: u32) {
    let path = root.join(package_path.trim_start_matches('/'));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, bytes).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
}

fn write_executable(root: &Path, package_path: &str) {
    write_regular(root, package_path, b"#!/bin/sh\nexit 0\n", 0o755);
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

fn symlink_node(target: &str) -> PayloadNode {
    numeric_node(
        PayloadNodeKind::Symlink {
            target: target.to_string(),
        },
        libc::S_IFLNK | 0o777,
    )
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
    let authority = PayloadContentAuthority {
        sha256: conary_core::hash::sha256(bytes),
        size: bytes.len() as u64,
    };
    PackagePayloadFile::new(
        path.to_string(),
        regular_node(mode),
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

/// Build one element's payload effects exactly as the install callers do.
fn effects(
    fixture: &Fixture,
    semantics: InstallSemantics,
    files: &[PackagePayloadFile],
) -> ElementPayloadEffects {
    effects_for(fixture, semantics, "fixture", None, files)
}

fn effects_for(
    fixture: &Fixture,
    semantics: InstallSemantics,
    package_name: &str,
    replacing_trove_id: Option<i64>,
    files: &[PackagePayloadFile],
) -> ElementPayloadEffects {
    plan_element_payload_effects(
        &fixture.conn,
        &fixture.root,
        ElementPayloadEffectInput {
            semantics,
            package_name,
            relation_removals: &[],
            replacing_trove_id,
            config_declarations: &[],
            files: PayloadEffectFiles::Extracted(files),
        },
    )
    .unwrap()
}

fn insert_claim(conn: &rusqlite::Connection, path: &str, node: PayloadNode, trove_id: i64) {
    let content = match &node.kind {
        PayloadNodeKind::Regular { .. } => Some(PayloadContentAuthority {
            sha256: conary_core::hash::sha256(b"#!/bin/sh\n"),
            size: 10,
        }),
        _ => None,
    };
    let mut entry = FileEntry::new(
        path.to_string(),
        ResolvedPayloadNode::from_numeric_source(node).unwrap(),
        content,
        trove_id,
    );
    entry.insert(conn).unwrap();
}

/// Insert one of two co-claimants sharing the same RPM-policy payload.
fn insert_shared_regular_claim(
    conn: &rusqlite::Connection,
    path: &str,
    trove_id: i64,
    first: bool,
) {
    let mut entry = FileEntry::new(
        path.to_string(),
        ResolvedPayloadNode::from_numeric_source(regular_node(0o755)).unwrap(),
        Some(PayloadContentAuthority {
            sha256: conary_core::hash::sha256(b"#!/bin/sh\n"),
            size: 10,
        }),
        trove_id,
    )
    .with_claim_policy(PayloadSharingPolicy::Rpm);
    if first {
        entry.insert(conn).unwrap();
    } else {
        entry
            .insert_or_replace(conn, ExistingDirectoryMaterialization::ApplyIncoming)
            .unwrap();
    }
}

fn trove(conn: &rusqlite::Connection, trove_id: i64) -> Trove {
    Trove::find_by_id(conn, trove_id).unwrap().unwrap()
}

fn plan(
    fixture: &Fixture,
    semantics: InstallSemantics,
    files: &[PackagePayloadFile],
    hooks: Vec<HookInterpreter>,
) -> ElementPlan {
    element_plan(
        "fixture",
        "1.0.0",
        None,
        &[],
        effects(fixture, semantics, files),
        hooks,
    )
}

/// Build one element plan whose payload declares a Debian conffile at `path`.
fn deb_conffile_plan(
    fixture: &Fixture,
    path: &str,
    files: &[PackagePayloadFile],
    hooks: Vec<HookInterpreter>,
) -> ElementPlan {
    let declarations = vec![SourceConfigDeclaration::Debian(DebianConfigDeclaration {
        control_index: 0,
        path: path.to_string(),
        remove_on_upgrade: false,
        payload: ConfigPayloadAssociation::Matched,
    })];
    element_plan(
        "fixture",
        "1.0.0",
        None,
        &[],
        plan_element_payload_effects(
            &fixture.conn,
            &fixture.root,
            ElementPayloadEffectInput {
                semantics: deb_semantics(),
                package_name: "fixture",
                relation_removals: &[],
                replacing_trove_id: None,
                config_declarations: &declarations,
                files: PayloadEffectFiles::Extracted(files),
            },
        )
        .unwrap(),
        hooks,
    )
}

fn live_file(path: &str, node: PayloadNode) -> LiveRootFile {
    LiveRootFile {
        path: path.to_string(),
        content: LiveRootContent::absent(),
        node: ResolvedPayloadNode::from_numeric_source(node).unwrap(),
    }
}

fn project_live_file(file: &LiveRootFile) -> ProjectedNode {
    projected_node(&file.node.source.kind, file.node.source.mode)
}

#[test]
fn projected_node_maps_every_payload_kind() {
    assert_eq!(
        project_live_file(&live_file("/usr/bin/run", regular_node(0o755))),
        ProjectedNode::Regular { executable: true }
    );
    assert_eq!(
        project_live_file(&live_file("/usr/share/data", regular_node(0o644))),
        ProjectedNode::Regular { executable: false }
    );
    assert_eq!(
        project_live_file(&live_file("/opt", directory_node(0o755))),
        ProjectedNode::Directory
    );
    assert_eq!(
        project_live_file(&live_file("/bin/sh", symlink_node("busybox"))),
        ProjectedNode::Symlink {
            target: "busybox".to_string()
        }
    );
    assert_eq!(
        project_live_file(&live_file(
            "/bin/sh",
            hardlink_node("/bin/busybox", "chain:1", 0o755)
        )),
        ProjectedNode::Hardlink {
            target: "/bin/busybox".to_string()
        }
    );
    assert_eq!(
        project_live_file(&live_file(
            "/run/pipe",
            numeric_node(PayloadNodeKind::Fifo, libc::S_IFIFO | 0o644)
        )),
        ProjectedNode::Other
    );
}

#[test]
fn post_install_interpreter_in_the_root_is_available() {
    let fixture = fixture();
    write_executable(&fixture.root, PRESENT);
    let element = plan(&fixture, rpm_semantics(), &[], vec![post_install(PRESENT)]);

    preflight_hook_interpreters(&fixture.conn, &fixture.root, &[element])
        .expect("an executable in the selected root is available");
}

#[test]
fn absent_post_install_interpreter_is_refused_with_the_exact_reason() {
    let fixture = fixture();
    write_executable(&fixture.root, PRESENT);
    let missing = "/usr/bin/absent";

    // Positive control on the same fixture: the present path is available.
    let present = plan(&fixture, rpm_semantics(), &[], vec![post_install(PRESENT)]);
    preflight_hook_interpreters(&fixture.conn, &fixture.root, &[present])
        .expect("the fixture's present executable is available");

    let element = plan(&fixture, rpm_semantics(), &[], vec![post_install(missing)]);
    let error = preflight_hook_interpreters(&fixture.conn, &fixture.root, &[element])
        .map_err(typed)
        .expect_err("an absent interpreter must be refused");
    assert_eq!(error.package, "fixture");
    assert_eq!(error.version, "1.0.0");
    assert_eq!(error.phase, HookPhase::PostInstall);
    assert_eq!(error.interpreter, missing);
}

#[test]
fn pre_remove_interpreter_is_refused_then_authorized_by_shipped_payload() {
    let fixture = fixture();

    let refused = plan(&fixture, rpm_semantics(), &[], vec![pre_remove("/bin/sh")]);
    let error = preflight_hook_interpreters(&fixture.conn, &fixture.root, &[refused])
        .map_err(typed)
        .expect_err("an unavailable pre-remove interpreter must be refused");
    assert_eq!(error.phase, HookPhase::PreRemove);
    assert_eq!(error.interpreter, "/bin/sh");

    // Positive control through the same fixture: the element ships /bin/sh.
    let satisfied = plan(
        &fixture,
        rpm_semantics(),
        &[regular_payload("/bin/sh", b"#!/bin/sh\n", 0o755)],
        vec![pre_remove("/bin/sh")],
    );
    preflight_hook_interpreters(&fixture.conn, &fixture.root, &[satisfied])
        .expect("a shipped payload interpreter satisfies the pre-remove hook");
}

#[test]
fn own_element_introduced_executable_authorizes_post_install() {
    let fixture = fixture();
    let planned = "/opt/planned/sh";

    // Negative control on the same fixture: without the element it is absent.
    let absent = plan(&fixture, rpm_semantics(), &[], vec![post_install(planned)]);
    preflight_hook_interpreters(&fixture.conn, &fixture.root, &[absent])
        .map_err(typed)
        .expect_err("an unplanned interpreter path must be refused");

    let provided = plan(
        &fixture,
        rpm_semantics(),
        &[regular_payload(planned, b"#!/bin/sh\n", 0o755)],
        vec![post_install(planned)],
    );
    preflight_hook_interpreters(&fixture.conn, &fixture.root, &[provided])
        .expect("a path introduced by this element is planned availability");
}

#[test]
fn a_later_element_provider_authorizes_an_earlier_consumer() {
    let fixture = fixture();
    let consumer = plan(
        &fixture,
        rpm_semantics(),
        &[],
        vec![post_install("/bin/sh")],
    );

    // Negative control: the consumer alone is refused.
    preflight_hook_interpreters(
        &fixture.conn,
        &fixture.root,
        std::slice::from_ref(&consumer),
    )
    .map_err(typed)
    .expect_err("a consumer with no provider must be refused");

    let provider = plan(
        &fixture,
        rpm_semantics(),
        &[regular_payload("/bin/sh", b"#!/bin/sh\n", 0o755)],
        Vec::new(),
    );
    preflight_hook_interpreters(&fixture.conn, &fixture.root, &[consumer, provider])
        .expect("a later element's payload authorizes an earlier element's interpreter");
}

#[test]
fn restore_removal_element_removes_a_root_provider_before_installs() {
    let fixture = fixture();
    write_executable(&fixture.root, PRESENT);
    let consumer = plan(&fixture, rpm_semantics(), &[], vec![post_install(PRESENT)]);

    // Positive control: before removal the root provider is available.
    preflight_hook_interpreters(
        &fixture.conn,
        &fixture.root,
        std::slice::from_ref(&consumer),
    )
    .expect("the root provider is available before removal");

    // A removal-only element that owns the interpreter path precedes the
    // install.
    let mut removal = removal_element_plan(&[]);
    removal.removed_paths = vec![PRESENT.to_string()];
    let error = preflight_hook_interpreters(&fixture.conn, &fixture.root, &[removal, consumer])
        .map_err(typed)
        .expect_err("a removed provider cannot authorize a restored hook");
    assert_eq!(error.interpreter, PRESENT);
    assert_eq!(error.phase, HookPhase::PostInstall);
}

#[test]
fn post_install_availability_does_not_authorize_a_different_pre_remove_interpreter() {
    let fixture = fixture();
    let consumer = plan(
        &fixture,
        rpm_semantics(),
        &[regular_payload(
            "/usr/bin/post-install-sh",
            b"#!/bin/sh\n",
            0o755,
        )],
        vec![
            post_install("/usr/bin/post-install-sh"),
            pre_remove("/bin/sh"),
        ],
    );

    let error = preflight_hook_interpreters(
        &fixture.conn,
        &fixture.root,
        std::slice::from_ref(&consumer),
    )
    .map_err(typed)
    .expect_err("the post-install interpreter must not authorize the pre-remove one");
    assert_eq!(error.phase, HookPhase::PreRemove);
    assert_eq!(error.interpreter, "/bin/sh");

    // Positive control through the same fixture: shipping the pre-remove
    // interpreter satisfies both hooks.
    let satisfied = plan(
        &fixture,
        rpm_semantics(),
        &[
            regular_payload("/usr/bin/post-install-sh", b"#!/bin/sh\n", 0o755),
            regular_payload("/bin/sh", b"#!/bin/sh\n", 0o755),
        ],
        vec![
            post_install("/usr/bin/post-install-sh"),
            pre_remove("/bin/sh"),
        ],
    );
    preflight_hook_interpreters(
        &fixture.conn,
        &fixture.root,
        std::slice::from_ref(&satisfied),
    )
    .expect("an element introducing both interpreters satisfies both phases");
}

#[test]
fn config_suffix_interpreter_is_refused_and_an_unmodified_primary_is_available() {
    let fixture = fixture();
    let old = b"old".to_vec();
    let owner = insert_trove(&fixture.conn, "previous-owner");
    let mut config = ConfigFile::new(
        "/etc/hook-interpreter".to_string(),
        owner,
        conary_core::hash::sha256(&old),
    );
    config.source = ConfigSource::Deb;
    config.insert(&fixture.conn).unwrap();

    // The primary is locally modified, so the incoming executable lands at
    // `.dpkg-dist` and the primary stays non-executable.
    write_regular(&fixture.root, "/etc/hook-interpreter", b"local", 0o644);
    let refused = deb_conffile_plan(
        &fixture,
        "/etc/hook-interpreter",
        &[regular_payload("/etc/hook-interpreter", b"new", 0o755)],
        vec![post_install("/etc/hook-interpreter")],
    );
    let error =
        preflight_hook_interpreters(&fixture.conn, &fixture.root, std::slice::from_ref(&refused))
            .map_err(typed)
            .expect_err("a suffixed config payload must not make the primary executable");
    assert_eq!(error.interpreter, "/etc/hook-interpreter");

    // Control through the same fixture: the primary is unmodified, so the
    // incoming executable replaces it and is available.
    write_regular(&fixture.root, "/etc/hook-interpreter", b"old", 0o644);
    let satisfied = deb_conffile_plan(
        &fixture,
        "/etc/hook-interpreter",
        &[regular_payload("/etc/hook-interpreter", b"new", 0o755)],
        vec![post_install("/etc/hook-interpreter")],
    );
    preflight_hook_interpreters(
        &fixture.conn,
        &fixture.root,
        std::slice::from_ref(&satisfied),
    )
    .expect("an unmodified primary is replaced by the executable");
}

#[test]
fn two_edge_hardlink_chain_is_available_and_a_non_executable_anchor_is_refused() {
    let fixture = fixture();
    let available = plan(
        &fixture,
        rpm_semantics(),
        &[
            regular_payload("/bin/anchor", b"#!/bin/sh\n", 0o755),
            hardlink_payload("/bin/edge", "/bin/anchor", "chain:1", 0o755),
            hardlink_payload("/bin/sh", "/bin/edge", "chain:1", 0o755),
        ],
        vec![post_install("/bin/sh")],
    );
    preflight_hook_interpreters(
        &fixture.conn,
        &fixture.root,
        std::slice::from_ref(&available),
    )
    .expect("a two-edge hardlink chain reaches the executable anchor");

    // Control through the same fixture: the anchor is not executable.
    let refused = plan(
        &fixture,
        rpm_semantics(),
        &[
            regular_payload("/bin/anchor", b"#!/bin/sh\n", 0o644),
            hardlink_payload("/bin/edge", "/bin/anchor", "chain:1", 0o644),
            hardlink_payload("/bin/sh", "/bin/edge", "chain:1", 0o644),
        ],
        vec![post_install("/bin/sh")],
    );
    let error =
        preflight_hook_interpreters(&fixture.conn, &fixture.root, std::slice::from_ref(&refused))
            .map_err(typed)
            .expect_err("a hardlink to a non-executable anchor is unavailable");
    assert_eq!(error.interpreter, "/bin/sh");
}

#[test]
fn directory_through_usr_merge_alias_reaches_a_shipped_interpreter() {
    let fixture = fixture();
    fs::create_dir_all(fixture.root.join("usr/bin")).unwrap();
    std::os::unix::fs::symlink("usr/bin", fixture.root.join("bin")).unwrap();

    // Positive control: the preserved `/bin -> usr/bin` alias plus the shipped
    // `/usr/bin/sh` makes a `/bin/sh` hook available.
    let available = plan(
        &fixture,
        rpm_semantics(),
        &[
            directory_payload("/bin", 0o755),
            regular_payload("/usr/bin/sh", b"#!/bin/sh\n", 0o755),
        ],
        vec![post_install("/bin/sh")],
    );
    preflight_hook_interpreters(
        &fixture.conn,
        &fixture.root,
        std::slice::from_ref(&available),
    )
    .expect("the preserved alias reaches the shipped interpreter");

    // Control through the same fixture: without the ship the alias names no
    // interpreter.
    let refused = plan(
        &fixture,
        rpm_semantics(),
        &[directory_payload("/bin", 0o755)],
        vec![post_install("/bin/sh")],
    );
    let error =
        preflight_hook_interpreters(&fixture.conn, &fixture.root, std::slice::from_ref(&refused))
            .map_err(typed)
            .expect_err("without the shipped interpreter the alias is unavailable");
    assert_eq!(error.interpreter, "/bin/sh");
}

#[test]
fn removed_alias_target_is_projected_at_its_effective_path() {
    let fixture = fixture();
    fs::create_dir_all(fixture.root.join("usr/bin")).unwrap();
    std::os::unix::fs::symlink("usr/bin", fixture.root.join("bin")).unwrap();
    write_executable(&fixture.root, "/usr/bin/sh");

    let owner = insert_trove(&fixture.conn, "alias-shell-owner");
    insert_claim(&fixture.conn, "/bin/sh", regular_node(0o755), owner);
    let owner_trove = trove(&fixture.conn, owner);

    // Negative: the transaction removes the alias spelling and ships nothing,
    // so the effective `/usr/bin/sh` must disappear with it.
    let refused = element_plan(
        "alias-shell-replacement",
        "2.0.0",
        Some(&owner_trove),
        &[],
        effects(&fixture, rpm_semantics(), &[]),
        vec![post_install("/bin/sh")],
    );
    let error = preflight_hook_interpreters(&fixture.conn, &fixture.root, &[refused])
        .map_err(typed)
        .expect_err("removing /bin/sh must hide the effective /usr/bin/sh");
    assert_eq!(error.interpreter, "/bin/sh");

    // Control: the incoming element re-provides the effective path.
    let admitted = element_plan(
        "alias-shell-replacement",
        "2.0.0",
        Some(&owner_trove),
        &[],
        effects(
            &fixture,
            rpm_semantics(),
            &[regular_payload("/usr/bin/sh", b"#!/bin/sh\n", 0o755)],
        ),
        vec![post_install("/bin/sh")],
    );
    preflight_hook_interpreters(&fixture.conn, &fixture.root, &[admitted])
        .expect("the incoming payload re-provides the effective interpreter");
}

#[test]
fn nonempty_removed_directory_is_retained_while_a_descendant_survives() {
    let fixture = fixture();
    write_executable(&fixture.root, "/opt/tools/sh");

    let directory_owner = insert_trove(&fixture.conn, "tools-directory-owner");
    insert_claim(
        &fixture.conn,
        "/opt/tools",
        directory_node(0o755),
        directory_owner,
    );
    let descendant_owner = insert_trove(&fixture.conn, "tools-descendant-owner");
    insert_claim(
        &fixture.conn,
        "/opt/tools/sh",
        regular_node(0o755),
        descendant_owner,
    );
    let directory_owner_trove = trove(&fixture.conn, directory_owner);
    let descendant_owner_trove = trove(&fixture.conn, descendant_owner);
    let consumer = plan(
        &fixture,
        rpm_semantics(),
        &[],
        vec![post_install("/opt/tools/sh")],
    );

    // The removed trove's sole directory claim must stay while a surviving
    // owner keeps the on-disk executable beneath it.
    let removal = removal_element_plan(std::slice::from_ref(&directory_owner_trove));
    preflight_hook_interpreters(&fixture.conn, &fixture.root, &[removal, consumer.clone()])
        .expect("a removed directory with a surviving descendant must stay");

    // Control: removing the descendant's owner too empties the directory.
    let removal = removal_element_plan(&[directory_owner_trove, descendant_owner_trove]);
    let error = preflight_hook_interpreters(&fixture.conn, &fixture.root, &[removal, consumer])
        .map_err(typed)
        .expect_err("removing the last descendant must release the directory");
    assert_eq!(error.interpreter, "/opt/tools/sh");
}

#[test]
fn final_incoming_claim_keeps_a_released_co_claimant_path() {
    let fixture = fixture();
    write_executable(&fixture.root, "/usr/bin/sh");
    let first = insert_trove(&fixture.conn, "shared-shell-first");
    let second = insert_trove(&fixture.conn, "shared-shell-second");
    insert_shared_regular_claim(&fixture.conn, "/usr/bin/sh", first, true);
    insert_shared_regular_claim(&fixture.conn, "/usr/bin/sh", second, false);
    let first_trove = trove(&fixture.conn, first);
    let second_trove = trove(&fixture.conn, second);

    // Control: no element keeps the path, so the last release wins.
    let dropped_first = element_plan(
        "shared-shell-first",
        "2.0.0",
        Some(&first_trove),
        &[],
        effects_for(
            &fixture,
            rpm_semantics(),
            "shared-shell-first",
            Some(first),
            &[],
        ),
        Vec::new(),
    );
    let dropped_second = element_plan(
        "shared-shell-second",
        "2.0.0",
        Some(&second_trove),
        &[],
        effects_for(
            &fixture,
            rpm_semantics(),
            "shared-shell-second",
            Some(second),
            &[],
        ),
        vec![post_install("/usr/bin/sh")],
    );
    let error = preflight_hook_interpreters(
        &fixture.conn,
        &fixture.root,
        &[dropped_first, dropped_second],
    )
    .map_err(typed)
    .expect_err("a co-claimed path no incoming element keeps must be released");
    assert_eq!(error.interpreter, "/usr/bin/sh");

    // The earlier element keeps the co-claimed path in the final incoming set,
    // so the later element's release must not win.
    let kept_first = element_plan(
        "shared-shell-first",
        "2.0.0",
        Some(&first_trove),
        &[],
        effects_for(
            &fixture,
            rpm_semantics(),
            "shared-shell-first",
            Some(first),
            &[regular_payload("/usr/bin/sh", b"#!/bin/sh\n", 0o755)],
        ),
        Vec::new(),
    );
    let dropped_second = element_plan(
        "shared-shell-second",
        "2.0.0",
        Some(&second_trove),
        &[],
        effects_for(
            &fixture,
            rpm_semantics(),
            "shared-shell-second",
            Some(second),
            &[],
        ),
        vec![post_install("/usr/bin/sh")],
    );
    preflight_hook_interpreters(&fixture.conn, &fixture.root, &[kept_first, dropped_second])
        .expect("the earlier element's incoming claim keeps the interpreter");
}
