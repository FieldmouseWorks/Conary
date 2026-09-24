// apps/conary/src/commands/install/ccs_hook_interpreter/tests.rs

#![cfg(test)]

use super::*;
use conary_core::packages::traits::ExtractedFile;
use conary_core::payload::{
    PayloadContentAuthority, PayloadIdentity, PayloadNode, PayloadNodeKind, PayloadTimestamp,
};
use std::fs;
use std::os::unix::fs::PermissionsExt;

const PRESENT: &str = "/usr/bin/hook-interpreter";

/// Unwrap the typed refusal; any other error fails the test.
fn typed(error: anyhow::Error) -> CcsHookInterpreterUnavailable {
    error
        .downcast()
        .expect("refusal must be the typed CcsHookInterpreterUnavailable")
}

fn executable(path: &str) -> IntroducedNode {
    IntroducedNode {
        path: path.to_string(),
        kind: IntroducedNodeKind::Executable,
    }
}

fn non_executable(path: &str) -> IntroducedNode {
    IntroducedNode {
        path: path.to_string(),
        kind: IntroducedNodeKind::NonExecutable,
    }
}

fn projected_symlink(path: &str, target: &str) -> IntroducedNode {
    IntroducedNode {
        path: path.to_string(),
        kind: IntroducedNodeKind::Symlink {
            target: target.to_string(),
        },
    }
}

fn payload_node(kind: PayloadNodeKind, mode: u32) -> PayloadNode {
    PayloadNode {
        kind,
        mode,
        user: PayloadIdentity::Numeric { id: 0 },
        group: PayloadIdentity::Numeric { id: 0 },
        mtime: PayloadTimestamp::UNIX_EPOCH,
        xattrs: Default::default(),
    }
}

fn payload_file(path: &str, node: PayloadNode) -> PackagePayloadFile {
    let content = b"#!/bin/sh\nexit 0\n".to_vec();
    let (content, content_authority) = if matches!(node.kind, PayloadNodeKind::Regular { .. }) {
        let authority = PayloadContentAuthority {
            sha256: conary_core::hash::sha256(&content),
            size: content.len() as u64,
        };
        (content, Some(authority))
    } else {
        (Vec::new(), None)
    };
    let payload =
        conary_core::packages::PackagePayload::from_extracted_in_memory(vec![ExtractedFile {
            path: path.to_string(),
            node,
            content,
            content_authority,
        }])
        .unwrap();
    payload.into_files().into_iter().next().unwrap()
}

fn ledger_with_executable() -> (tempfile::TempDir, HookInterpreterLedger) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join(PRESENT.trim_start_matches('/'));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    let ledger = HookInterpreterLedger::new(root.path());
    (root, ledger)
}

#[test]
fn present_interpreter_in_root_is_available() {
    let (_root, ledger) = ledger_with_executable();

    ledger
        .require("pkg", "1.0.0", HookPhase::PostInstall, PRESENT)
        .expect("an executable in the selected root is available");
}

#[test]
fn absent_interpreter_is_rejected_with_the_exact_reason() {
    let (_root, ledger) = ledger_with_executable();
    let missing = "/usr/bin/absent";

    // Positive control: the identical fixture finds the present path.
    ledger
        .require("pkg", "1.0.0", HookPhase::PostInstall, PRESENT)
        .expect("the fixture's present executable is available");

    let error = ledger
        .require("pkg", "1.0.0", HookPhase::PostInstall, missing)
        .map_err(typed)
        .expect_err("an absent interpreter must be refused");
    assert_eq!(error.package, "pkg");
    assert_eq!(error.version, "1.0.0");
    assert_eq!(error.phase, HookPhase::PostInstall);
    assert_eq!(error.interpreter, missing);
}

#[test]
fn own_element_can_introduce_an_absent_interpreter() {
    let root = tempfile::tempdir().unwrap();
    let mut ledger = HookInterpreterLedger::new(root.path());
    let planned = "/opt/planned/sh";

    // Negative control on the same fixture: without the element it is absent.
    assert!(
        ledger
            .require("pkg", "1.0.0", HookPhase::PostInstall, planned)
            .map_err(typed)
            .is_err()
    );

    ledger
        .apply_element(Vec::new(), vec![executable(planned)], Vec::new())
        .unwrap();
    ledger
        .require("pkg", "1.0.0", HookPhase::PostInstall, planned)
        .expect("a path introduced by this element is planned availability");
}

#[test]
fn earlier_element_executable_authorizes_a_later_interpreter() {
    let root = tempfile::tempdir().unwrap();
    let mut ledger = HookInterpreterLedger::new(root.path());

    ledger
        .apply_element(Vec::new(), vec![executable("/usr/bin/sh")], Vec::new())
        .unwrap();
    ledger
        .require(
            "provider-dependent",
            "2.0.0",
            HookPhase::PostInstall,
            "/usr/bin/sh",
        )
        .expect("an earlier element's executable payload authorizes the interpreter");
}

#[test]
fn removal_by_an_earlier_element_beats_root_presence() {
    let (_root, mut ledger) = ledger_with_executable();

    // Positive control: before removal the same fixture is available.
    ledger
        .require("pkg", "1.0.0", HookPhase::PostInstall, PRESENT)
        .expect("the fixture's executable is available before removal");

    ledger
        .apply_element(vec![PRESENT.to_string()], Vec::new(), Vec::new())
        .unwrap();
    let error = ledger
        .require("pkg", "1.0.0", HookPhase::PostInstall, PRESENT)
        .map_err(typed)
        .expect_err("a removed final provider must not authorize a later interpreter");
    assert_eq!(error.interpreter, PRESENT);
    assert_eq!(error.phase, HookPhase::PostInstall);
}

#[test]
fn reintroduction_after_removal_restores_availability() {
    let root = tempfile::tempdir().unwrap();
    let mut ledger = HookInterpreterLedger::new(root.path());

    ledger
        .apply_element(vec![PRESENT.to_string()], Vec::new(), Vec::new())
        .unwrap();
    assert!(
        ledger
            .require("pkg", "1.0.0", HookPhase::PostInstall, PRESENT)
            .map_err(typed)
            .is_err()
    );

    ledger
        .apply_element(Vec::new(), vec![executable(PRESENT)], Vec::new())
        .unwrap();
    ledger
        .require("pkg", "1.0.0", HookPhase::PostInstall, PRESENT)
        .expect("a later element reintroducing the path restores availability");
}

#[test]
fn introduced_node_classification_uses_kind_and_mode() {
    let classify = |node| IntroducedNode::from_payload_file(&payload_file("/usr/bin/node", node));

    assert_eq!(
        classify(PayloadNode::regular(0o755)).kind,
        IntroducedNodeKind::Executable
    );
    assert_eq!(
        classify(PayloadNode::regular(0o644)).kind,
        IntroducedNodeKind::NonExecutable
    );
    assert_eq!(
        classify(payload_node(
            PayloadNodeKind::Directory,
            libc::S_IFDIR | 0o755
        ))
        .kind,
        IntroducedNodeKind::Directory
    );
    assert_eq!(
        classify(payload_node(
            PayloadNodeKind::Symlink {
                target: "busybox".to_string(),
            },
            libc::S_IFLNK | 0o777,
        ))
        .kind,
        IntroducedNodeKind::Symlink {
            target: "busybox".to_string(),
        }
    );
}

#[test]
fn introduced_non_executable_regular_file_is_unavailable_and_exec_bit_authorizes() {
    let root = tempfile::tempdir().unwrap();
    let mut ledger = HookInterpreterLedger::new(root.path());
    let path = "/usr/bin/hook-interpreter";
    ledger
        .apply_element(Vec::new(), vec![non_executable(path)], Vec::new())
        .unwrap();
    let error = ledger
        .require("pkg", "1.0.0", HookPhase::PostInstall, path)
        .map_err(typed)
        .expect_err("a regular payload file without an execute bit cannot run the hook");
    assert_eq!(error.interpreter, path);

    // Positive control on the same fixture: the execute bit authorizes.
    let mut executable_ledger = HookInterpreterLedger::new(root.path());
    executable_ledger
        .apply_element(Vec::new(), vec![executable(path)], Vec::new())
        .unwrap();
    executable_ledger
        .require("pkg", "1.0.0", HookPhase::PostInstall, path)
        .expect("the same node with an execute bit authorizes the interpreter");
}

#[test]
fn introduced_directory_at_the_interpreter_path_is_unavailable() {
    let root = tempfile::tempdir().unwrap();
    let mut ledger = HookInterpreterLedger::new(root.path());
    let directory = IntroducedNode::from_payload_file(&payload_file(
        "/usr/bin/sh",
        payload_node(PayloadNodeKind::Directory, libc::S_IFDIR | 0o755),
    ));
    ledger
        .apply_element(Vec::new(), vec![directory], Vec::new())
        .unwrap();
    let error = ledger
        .require("pkg", "1.0.0", HookPhase::PostInstall, "/usr/bin/sh")
        .map_err(typed)
        .expect_err("a directory node cannot act as the hook interpreter");
    assert_eq!(error.interpreter, "/usr/bin/sh");

    // Positive control on the same fixture: a regular executable node at the
    // same path authorizes.
    let executable_file = IntroducedNode::from_payload_file(&payload_file(
        "/usr/bin/sh",
        PayloadNode::regular(0o755),
    ));
    let mut executable_ledger = HookInterpreterLedger::new(root.path());
    executable_ledger
        .apply_element(Vec::new(), vec![executable_file], Vec::new())
        .unwrap();
    executable_ledger
        .require("pkg", "1.0.0", HookPhase::PostInstall, "/usr/bin/sh")
        .expect("the same fixture with an executable regular node authorizes");
}

#[test]
fn introduced_directory_nodes_are_traversed_to_reach_a_child_executable() {
    let root = tempfile::tempdir().unwrap();
    let mut ledger = HookInterpreterLedger::new(root.path());
    let directory = |path: &str| {
        IntroducedNode::from_payload_file(&payload_file(
            path,
            payload_node(PayloadNodeKind::Directory, libc::S_IFDIR | 0o755),
        ))
    };
    ledger
        .apply_element(
            Vec::new(),
            vec![
                directory("/usr"),
                directory("/usr/bin"),
                executable("/usr/bin/sh"),
            ],
            Vec::new(),
        )
        .unwrap();
    ledger
        .require("pkg", "1.0.0", HookPhase::PostInstall, "/usr/bin/sh")
        .expect("payload directory nodes must be traversed to reach the interpreter");
}

#[test]
fn introduced_symlink_to_an_introduced_executable_is_available() {
    let root = tempfile::tempdir().unwrap();
    let mut ledger = HookInterpreterLedger::new(root.path());
    ledger
        .apply_element(
            Vec::new(),
            vec![
                projected_symlink("/bin/sh", "busybox"),
                executable("/bin/busybox"),
            ],
            Vec::new(),
        )
        .unwrap();
    ledger
        .require("pkg", "1.0.0", HookPhase::PostInstall, "/bin/sh")
        .expect("an introduced symlink to an introduced executable is available");
}

#[test]
fn introduced_symlink_absolute_target_resolves_inside_the_root() {
    let root = tempfile::tempdir().unwrap();
    let mut ledger = HookInterpreterLedger::new(root.path());
    ledger
        .apply_element(
            Vec::new(),
            vec![
                projected_symlink("/bin/sh", "/usr/bin/busybox"),
                executable("/usr/bin/busybox"),
            ],
            Vec::new(),
        )
        .unwrap();
    ledger
        .require("pkg", "1.0.0", HookPhase::PostInstall, "/bin/sh")
        .expect("an absolute symlink target is root-relative, never host-relative");
}

#[test]
fn introduced_symlink_without_a_projected_target_is_unavailable() {
    let root = tempfile::tempdir().unwrap();
    let mut ledger = HookInterpreterLedger::new(root.path());
    ledger
        .apply_element(
            Vec::new(),
            vec![projected_symlink("/bin/sh", "busybox")],
            Vec::new(),
        )
        .unwrap();
    let error = ledger
        .require("pkg", "1.0.0", HookPhase::PostInstall, "/bin/sh")
        .map_err(typed)
        .expect_err("an introduced symlink with no provided target cannot run the hook");
    assert_eq!(error.interpreter, "/bin/sh");

    // Positive control on the same fixture: providing the target authorizes.
    let mut provided = HookInterpreterLedger::new(root.path());
    provided
        .apply_element(
            Vec::new(),
            vec![
                projected_symlink("/bin/sh", "busybox"),
                executable("/bin/busybox"),
            ],
            Vec::new(),
        )
        .unwrap();
    provided
        .require("pkg", "1.0.0", HookPhase::PostInstall, "/bin/sh")
        .expect("the same symlink with its executable target provided authorizes");
}

#[test]
fn declared_file_capability_without_a_payload_node_does_not_authorize() {
    let root = tempfile::tempdir().unwrap();
    let mut ledger = HookInterpreterLedger::new(root.path());
    ledger
        .apply_element(Vec::new(), Vec::new(), vec!["/bin/sh".to_string()])
        .unwrap();
    let error = ledger
        .require("pkg", "1.0.0", HookPhase::PostInstall, "/bin/sh")
        .map_err(typed)
        .expect_err("a declared File capability is not payload authority");
    assert_eq!(error.interpreter, "/bin/sh");

    // Positive control on the same fixture: a payload-backed executable
    // authorizes even though the declaration alone did not.
    let mut backed = HookInterpreterLedger::new(root.path());
    backed
        .apply_element(
            Vec::new(),
            vec![executable("/bin/sh")],
            vec!["/bin/sh".to_string()],
        )
        .unwrap();
    backed
        .require("pkg", "1.0.0", HookPhase::PostInstall, "/bin/sh")
        .expect("a declared File capability backed by an executable node authorizes");
}

#[test]
fn selected_root_alias_normalizes_both_payload_and_interpreter() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("usr/bin")).unwrap();
    std::os::unix::fs::symlink("usr/bin", root.path().join("bin")).unwrap();
    let mut ledger = HookInterpreterLedger::new(root.path());
    ledger
        .apply_element(Vec::new(), vec![executable("/usr/bin/sh")], Vec::new())
        .unwrap();
    ledger
        .require("pkg", "1.0.0", HookPhase::PostInstall, "/bin/sh")
        .expect("the selected-root alias must resolve to the introduced payload path");

    // Negative control on the same fixture: an alias the element does not
    // introduce is still absent.
    let error = ledger
        .require("pkg", "1.0.0", HookPhase::PostInstall, "/bin/bash")
        .map_err(typed)
        .expect_err("an alias that the transaction does not provide is refused");
    assert_eq!(error.interpreter, "/bin/bash");
}

#[test]
fn removed_ancestor_symlink_makes_the_interpreter_unreachable() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("usr/bin")).unwrap();
    std::os::unix::fs::symlink("usr/bin", root.path().join("bin")).unwrap();
    let installed = root.path().join("usr/bin/sh");
    fs::write(&installed, b"#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&installed, fs::Permissions::from_mode(0o755)).unwrap();
    let mut ledger = HookInterpreterLedger::new(root.path());

    // Positive control on the same fixture: the root alias reaches the
    // executable before any removal.
    ledger
        .require("pkg", "1.0.0", HookPhase::PostInstall, "/bin/sh")
        .expect("the root alias reaches the executable before removal");

    ledger
        .apply_element(vec!["/bin".to_string()], Vec::new(), Vec::new())
        .unwrap();
    let error = ledger
        .require("pkg", "1.0.0", HookPhase::PostInstall, "/bin/sh")
        .map_err(typed)
        .expect_err("a removed ancestor symlink must make the alias unreachable");
    assert_eq!(error.interpreter, "/bin/sh");
}

#[test]
fn introduced_ancestor_symlink_reaches_an_introduced_executable() {
    let root = tempfile::tempdir().unwrap();
    let mut ledger = HookInterpreterLedger::new(root.path());
    ledger
        .apply_element(
            Vec::new(),
            vec![
                projected_symlink("/bin", "usr/bin"),
                executable("/usr/bin/sh"),
            ],
            Vec::new(),
        )
        .unwrap();
    ledger
        .require("pkg", "1.0.0", HookPhase::PostInstall, "/bin/sh")
        .expect("an introduced ancestor symlink reaches the introduced executable");

    // Negative control on the same fixture: without the introduced symlink the
    // root has no /bin to resolve the interpreter through.
    let mut without_alias = HookInterpreterLedger::new(root.path());
    without_alias
        .apply_element(Vec::new(), vec![executable("/usr/bin/sh")], Vec::new())
        .unwrap();
    let error = without_alias
        .require("pkg", "1.0.0", HookPhase::PostInstall, "/bin/sh")
        .map_err(typed)
        .expect_err("without the projected symlink the interpreter path is unreachable");
    assert_eq!(error.interpreter, "/bin/sh");
}

#[test]
fn projected_and_root_symlink_loop_is_a_resolution_error() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("usr")).unwrap();
    std::os::unix::fs::symlink("usr/bin", root.path().join("bin")).unwrap();
    let mut ledger = HookInterpreterLedger::new(root.path());
    ledger
        .apply_element(
            Vec::new(),
            vec![projected_symlink("/usr/bin", "/bin")],
            Vec::new(),
        )
        .unwrap();

    let error = ledger
        .require("pkg", "1.0.0", HookPhase::PostInstall, "/bin/sh")
        .expect_err("a projected/root symlink loop must fail resolution");
    assert!(
        error
            .downcast_ref::<CcsHookInterpreterUnavailable>()
            .is_none(),
        "a symlink loop is a resolution error, not the typed unavailability"
    );
    assert!(
        matches!(
            error.downcast_ref::<conary_core::Error>(),
            Some(conary_core::Error::PathTraversal(_))
        ),
        "the loop must surface as a typed selected-root path traversal: {error:?}"
    );
}

#[test]
fn preflight_records_every_element_before_requiring_any_interpreter() {
    let root = tempfile::tempdir().unwrap();
    let elements = vec![
        ElementPlan {
            package: "consumer".to_string(),
            version: "1.0.0".to_string(),
            removed_trove_ids: Vec::new(),
            removed_paths: Vec::new(),
            introduced_nodes: Vec::new(),
            declared_file_capabilities: Vec::new(),
            post_install_interpreter: Some("/bin/sh".to_string()),
        },
        ElementPlan {
            package: "provider".to_string(),
            version: "1.0.0".to_string(),
            removed_trove_ids: Vec::new(),
            removed_paths: Vec::new(),
            introduced_nodes: vec![executable("/bin/sh")],
            declared_file_capabilities: Vec::new(),
            post_install_interpreter: None,
        },
    ];
    preflight_post_install_interpreters(&test_conn().1, root.path(), &elements)
        .expect("a later element's payload authorizes an earlier element's interpreter");
}

/// A fresh database; element plans without removed troves never query it.
fn test_conn() -> (tempfile::TempDir, rusqlite::Connection) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("conary.db");
    conary_core::db::init(path.to_str().unwrap()).unwrap();
    let conn = conary_core::db::open(path.to_str().unwrap()).unwrap();
    (temp, conn)
}

fn projected_hardlink(path: &str, target: &str) -> IntroducedNode {
    IntroducedNode {
        path: path.to_string(),
        kind: IntroducedNodeKind::Hardlink {
            target: target.to_string(),
        },
    }
}

#[test]
fn introduced_hardlink_shares_its_target_node_executability() {
    // Busybox-style providers hardlink applets to one executable.
    let root = tempfile::tempdir().unwrap();
    let mut ledger = HookInterpreterLedger::new(root.path());
    ledger
        .apply_element(
            Vec::new(),
            vec![
                executable("/bin/busybox"),
                projected_hardlink("/bin/sh", "/bin/busybox"),
            ],
            Vec::new(),
        )
        .unwrap();
    ledger
        .require("pkg", "1.0.0", HookPhase::PostInstall, "/bin/sh")
        .expect("a hardlink to an introduced executable is available");

    // Same shape, but the shared inode is not executable.
    let root = tempfile::tempdir().unwrap();
    let mut ledger = HookInterpreterLedger::new(root.path());
    ledger
        .apply_element(
            Vec::new(),
            vec![
                non_executable("/bin/busybox"),
                projected_hardlink("/bin/sh", "/bin/busybox"),
            ],
            Vec::new(),
        )
        .unwrap();
    let error = ledger
        .require("pkg", "1.0.0", HookPhase::PostInstall, "/bin/sh")
        .map_err(typed)
        .expect_err("a hardlink to a non-executable node is unavailable");
    assert_eq!(error.interpreter, "/bin/sh");
}

#[test]
fn restore_removal_element_removes_a_root_provider_before_installs() {
    let (_root, ledger) = ledger_with_executable();
    let root_path = ledger.root.clone();

    // Positive control: with no removal element the root interpreter is available.
    let consumer = ElementPlan {
        package: "consumer".to_string(),
        version: "1.0.0".to_string(),
        removed_trove_ids: Vec::new(),
        removed_paths: Vec::new(),
        introduced_nodes: Vec::new(),
        declared_file_capabilities: Vec::new(),
        post_install_interpreter: Some(PRESENT.to_string()),
    };
    preflight_post_install_interpreters(
        &test_conn().1,
        &root_path,
        std::slice::from_ref(&consumer),
    )
    .expect("the root interpreter is available without a removal");

    // A removal-only element that owns the interpreter path precedes the install.
    let removal = ElementPlan {
        package: String::new(),
        version: String::new(),
        removed_trove_ids: Vec::new(),
        removed_paths: vec![PRESENT.to_string()],
        introduced_nodes: Vec::new(),
        declared_file_capabilities: Vec::new(),
        post_install_interpreter: None,
    };
    let error =
        preflight_post_install_interpreters(&test_conn().1, &root_path, &[removal, consumer])
            .map_err(typed)
            .expect_err("a removed provider cannot authorize a restored hook");
    assert_eq!(error.package, "consumer");
    assert_eq!(error.interpreter, PRESENT);
}

#[test]
fn implied_parent_defers_to_an_existing_root_symlink() {
    // Root: `/bin -> usr/bin` with an executable `/usr/bin/sh`. An element
    // introduces an unrelated `/bin/tool`; materialization writes through the
    // existing symlink, so `/bin` must not become a shadowing directory.
    let root = tempfile::tempdir().unwrap();
    let sh = root.path().join("usr/bin/sh");
    fs::create_dir_all(sh.parent().unwrap()).unwrap();
    fs::write(&sh, b"#!/bin/sh\n").unwrap();
    fs::set_permissions(&sh, fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink("usr/bin", root.path().join("bin")).unwrap();
    let mut ledger = HookInterpreterLedger::new(root.path());

    // Positive control: before any element, `/bin/sh` resolves through root.
    ledger
        .require("pkg", "1.0.0", HookPhase::PostInstall, "/bin/sh")
        .expect("root alias resolves the interpreter");

    ledger
        .apply_element(Vec::new(), vec![executable("/bin/tool")], Vec::new())
        .unwrap();
    ledger
        .require("pkg", "1.0.0", HookPhase::PostInstall, "/bin/sh")
        .expect("an implied /bin parent must not shadow the root symlink");
}

#[test]
fn removed_parent_recreated_by_payload_is_a_real_directory() {
    // Root: `/bin -> usr/bin`, no `/usr/bin/sh`. The transaction removes the
    // `/bin` symlink, then a payload ships `/bin/sh` itself: materialization
    // recreates `/bin` as a directory holding the new executable.
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("usr/bin")).unwrap();
    std::os::unix::fs::symlink("usr/bin", root.path().join("bin")).unwrap();
    let mut ledger = HookInterpreterLedger::new(root.path());

    ledger
        .apply_element(vec!["/bin".to_string()], Vec::new(), Vec::new())
        .unwrap();
    // Negative control: with `/bin` removed and nothing shipped, unreachable.
    let error = ledger
        .require("pkg", "1.0.0", HookPhase::PostInstall, "/bin/sh")
        .map_err(typed)
        .expect_err("a removed ancestor makes the interpreter unreachable");
    assert_eq!(error.interpreter, "/bin/sh");

    ledger
        .apply_element(Vec::new(), vec![executable("/bin/sh")], Vec::new())
        .unwrap();
    ledger
        .require("pkg", "1.0.0", HookPhase::PostInstall, "/bin/sh")
        .expect("the payload recreates /bin as a directory with an executable sh");
}
