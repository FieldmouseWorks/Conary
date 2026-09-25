// crates/conary-core/src/filesystem/selected_root/projection/tests.rs

#![cfg(test)]

use super::*;
use crate::error::Error;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

fn executable() -> ProjectedNode {
    ProjectedNode::Regular { executable: true }
}

fn non_executable() -> ProjectedNode {
    ProjectedNode::Regular { executable: false }
}

fn symlink_node(target: &str) -> ProjectedNode {
    ProjectedNode::Symlink {
        target: target.to_string(),
    }
}

fn hardlink(target: &str) -> ProjectedNode {
    ProjectedNode::Hardlink {
        target: target.to_string(),
    }
}

fn executable_outcome(resolved: &str) -> ProjectedExecutable {
    ProjectedExecutable::Executable {
        resolved: resolved.to_string(),
    }
}

fn not_executable_outcome(resolved: &str) -> ProjectedExecutable {
    ProjectedExecutable::NotExecutable {
        resolved: resolved.to_string(),
    }
}

fn write_regular(root: &Path, relative: &str, mode: u32) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
}

fn write_executable(root: &Path, relative: &str) {
    write_regular(root, relative, 0o755);
}

fn root_symlink(root: &Path, relative: &str, target: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    std::os::unix::fs::symlink(target, path).unwrap();
}

#[test]
fn root_alias_reaches_an_introduced_executable() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("usr/bin")).unwrap();
    root_symlink(root.path(), "bin", "usr/bin");

    let mut projection = SelectedRootProjection::new(root.path());
    projection.insert("/usr/bin/sh", executable()).unwrap();

    assert_eq!(
        projection.resolve_executable("/bin/sh").unwrap(),
        executable_outcome("/usr/bin/sh")
    );

    // Negative control on the same fixture: the alias does not invent a path
    // the projection never introduced.
    assert_eq!(
        projection.resolve_executable("/bin/bash").unwrap(),
        ProjectedExecutable::Missing
    );
}

#[test]
fn absolute_symlink_target_outside_the_selected_root_is_missing() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("bin")).unwrap();
    root_symlink(root.path(), "bin/sh", "/usr/bin/env");
    assert!(
        Path::new("/usr/bin/env").exists(),
        "the host fixture /usr/bin/env must exist for the no-escape proof"
    );

    let projection = SelectedRootProjection::new(root.path());
    assert_eq!(
        projection.resolve_executable("/bin/sh").unwrap(),
        ProjectedExecutable::Missing
    );

    // Positive control on the same fixture: providing the absolute target
    // inside the selected root resolves it.
    write_executable(root.path(), "usr/bin/env");
    assert_eq!(
        SelectedRootProjection::new(root.path())
            .resolve_executable("/bin/sh")
            .unwrap(),
        executable_outcome("/usr/bin/env")
    );
}

#[test]
fn overlay_absolute_symlink_target_outside_the_projection_is_missing() {
    let root = tempfile::tempdir().unwrap();
    assert!(
        Path::new("/usr/bin/env").exists(),
        "the host fixture /usr/bin/env must exist for the no-escape proof"
    );

    let mut projection = SelectedRootProjection::new(root.path());
    projection
        .insert("/bin/sh", symlink_node("/usr/bin/env"))
        .unwrap();
    assert_eq!(
        projection.resolve_executable("/bin/sh").unwrap(),
        ProjectedExecutable::Missing
    );

    // Positive control on the same fixture: introducing the target resolves it.
    let mut provided = SelectedRootProjection::new(root.path());
    provided
        .insert("/bin/sh", symlink_node("/usr/bin/env"))
        .unwrap();
    provided.insert("/usr/bin/env", executable()).unwrap();
    assert_eq!(
        provided.resolve_executable("/bin/sh").unwrap(),
        executable_outcome("/usr/bin/env")
    );
}

#[test]
fn removal_shadows_an_on_disk_executable() {
    let root = tempfile::tempdir().unwrap();
    write_executable(root.path(), "usr/bin/sh");

    let mut projection = SelectedRootProjection::new(root.path());
    // Positive control before removal.
    assert_eq!(
        projection.resolve_executable("/usr/bin/sh").unwrap(),
        executable_outcome("/usr/bin/sh")
    );

    projection.remove("/usr/bin/sh").unwrap();
    assert_eq!(
        projection.resolve_executable("/usr/bin/sh").unwrap(),
        ProjectedExecutable::Missing
    );
}

#[test]
fn removed_ancestor_symlink_makes_its_target_unreachable() {
    let root = tempfile::tempdir().unwrap();
    write_executable(root.path(), "usr/bin/sh");
    root_symlink(root.path(), "bin", "usr/bin");

    let mut projection = SelectedRootProjection::new(root.path());
    // Positive control before the ancestor removal.
    assert_eq!(
        projection.resolve_executable("/bin/sh").unwrap(),
        executable_outcome("/usr/bin/sh")
    );

    projection.remove("/bin").unwrap();
    assert_eq!(
        projection.resolve_executable("/bin/sh").unwrap(),
        ProjectedExecutable::Missing
    );
}

#[test]
fn implied_parents_materialize_a_path_absent_from_the_root() {
    let root = tempfile::tempdir().unwrap();
    let mut projection = SelectedRootProjection::new(root.path());
    projection.insert("/opt/tools/run", executable()).unwrap();

    assert_eq!(
        projection.resolve_executable("/opt/tools/run").unwrap(),
        executable_outcome("/opt/tools/run")
    );

    // Negative control through the same implied parents: a sibling the
    // projection never introduced is still missing.
    assert_eq!(
        projection.resolve_executable("/opt/tools/absent").unwrap(),
        ProjectedExecutable::Missing
    );
}

#[test]
fn implied_parent_defers_to_an_on_disk_symlink() {
    let root = tempfile::tempdir().unwrap();
    write_executable(root.path(), "usr/bin/sh");
    root_symlink(root.path(), "bin", "usr/bin");

    let mut projection = SelectedRootProjection::new(root.path());
    projection.insert("/bin/tool", executable()).unwrap();

    // The introduced /bin/tool writes through the existing alias rather than
    // shadowing it as a directory, so an unrelated path behind the alias still
    // resolves through the root.
    assert_eq!(
        projection.resolve_executable("/bin/sh").unwrap(),
        executable_outcome("/usr/bin/sh")
    );
}

#[test]
fn removed_parent_recreated_by_a_later_payload_is_a_real_directory() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("usr/bin")).unwrap();
    root_symlink(root.path(), "bin", "usr/bin");

    let mut projection = SelectedRootProjection::new(root.path());
    projection.remove("/bin").unwrap();
    assert_eq!(
        projection.resolve_executable("/bin/sh").unwrap(),
        ProjectedExecutable::Missing
    );

    // Positive control on the same fixture: the later payload recreates /bin
    // as a real directory holding the new executable.
    projection.insert("/bin/sh", executable()).unwrap();
    assert_eq!(
        projection.resolve_executable("/bin/sh").unwrap(),
        executable_outcome("/bin/sh")
    );
}

#[test]
fn hardlink_chain_resolves_to_its_regular_anchor() {
    let root = tempfile::tempdir().unwrap();
    let mut projection = SelectedRootProjection::new(root.path());
    projection.insert("/bin/busybox", executable()).unwrap();
    projection
        .insert("/bin/sh", hardlink("/bin/busybox"))
        .unwrap();
    assert_eq!(
        projection.resolve_executable("/bin/sh").unwrap(),
        executable_outcome("/bin/busybox")
    );
    // Positive control: the anchor resolves on its own.
    assert_eq!(
        projection.resolve_executable("/bin/busybox").unwrap(),
        executable_outcome("/bin/busybox")
    );
}

#[test]
fn hardlink_chains_of_any_length_resolve_to_the_anchor() {
    let root = tempfile::tempdir().unwrap();
    let mut projection = SelectedRootProjection::new(root.path());
    projection.insert("/bin/tool", executable()).unwrap();
    projection
        .insert("/bin/busybox", hardlink("/bin/tool"))
        .unwrap();
    projection
        .insert("/bin/sh", hardlink("/bin/busybox"))
        .unwrap();

    assert_eq!(
        projection.resolve_executable("/bin/sh").unwrap(),
        executable_outcome("/bin/tool")
    );

    // Negative control on the same fixture: a hardlink to a non-executable
    // anchor is not executable.
    projection.insert("/bin/data", non_executable()).unwrap();
    projection
        .insert("/bin/tool", hardlink("/bin/data"))
        .unwrap();
    assert_eq!(
        projection.resolve_executable("/bin/sh").unwrap(),
        not_executable_outcome("/bin/data")
    );
}

#[test]
fn hardlink_cycle_is_a_resolution_error() {
    let root = tempfile::tempdir().unwrap();
    let mut projection = SelectedRootProjection::new(root.path());
    projection.insert("/a", hardlink("/b")).unwrap();
    projection.insert("/b", hardlink("/a")).unwrap();

    let error = projection.resolve_executable("/a").unwrap_err();
    assert!(matches!(error, Error::PathTraversal(_)), "{error}");

    // Positive control on the same fixture: a terminating chain resolves once
    // the cycle is replaced by a regular anchor.
    projection.insert("/c", executable()).unwrap();
    projection.insert("/b", hardlink("/c")).unwrap();
    assert_eq!(
        projection.resolve_executable("/a").unwrap(),
        executable_outcome("/c")
    );
}

#[test]
fn symlink_depth_overflow_is_a_resolution_error() {
    // Positive control: a chain exactly at the bound resolves.
    let within = tempfile::tempdir().unwrap();
    for index in 0..MAX_SELECTED_ROOT_SYMLINK_DEPTH {
        root_symlink(
            within.path(),
            &format!("l{index}"),
            &format!("l{}", index + 1),
        );
    }
    let terminal = MAX_SELECTED_ROOT_SYMLINK_DEPTH;
    write_executable(within.path(), &format!("l{terminal}"));
    assert_eq!(
        SelectedRootProjection::new(within.path())
            .resolve_executable("/l0")
            .unwrap(),
        executable_outcome(&format!("/l{terminal}"))
    );

    // One more redirect overflows the bound and is an error, not Missing.
    let overflow = tempfile::tempdir().unwrap();
    for index in 0..=MAX_SELECTED_ROOT_SYMLINK_DEPTH {
        root_symlink(
            overflow.path(),
            &format!("l{index}"),
            &format!("l{}", index + 1),
        );
    }
    write_executable(overflow.path(), &format!("l{}", terminal + 1));
    let error = SelectedRootProjection::new(overflow.path())
        .resolve_executable("/l0")
        .unwrap_err();
    assert!(matches!(error, Error::PathTraversal(_)), "{error}");
}

#[test]
fn mixed_projected_and_root_symlink_loop_is_a_resolution_error() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("usr")).unwrap();
    root_symlink(root.path(), "bin", "usr/bin");

    let mut projection = SelectedRootProjection::new(root.path());
    projection.insert("/usr/bin", symlink_node("/bin")).unwrap();

    let error = projection.resolve_executable("/bin/sh").unwrap_err();
    assert!(matches!(error, Error::PathTraversal(_)), "{error}");

    // Positive control on the same on-disk fixture: without the projected loop
    // the root alias reaches an executable. A fresh projection is required;
    // `remove` would tombstone `/usr/bin` and hide the on-disk directory too.
    write_executable(root.path(), "usr/bin/sh");
    let projection = SelectedRootProjection::new(root.path());
    assert_eq!(
        projection.resolve_executable("/bin/sh").unwrap(),
        executable_outcome("/usr/bin/sh")
    );
}

#[test]
fn overlay_relative_symlink_reaches_an_overlay_executable() {
    let root = tempfile::tempdir().unwrap();
    let mut projection = SelectedRootProjection::new(root.path());
    projection
        .insert("/bin/sh", symlink_node("busybox"))
        .unwrap();
    projection.insert("/bin/busybox", executable()).unwrap();

    assert_eq!(
        projection.resolve_executable("/bin/sh").unwrap(),
        executable_outcome("/bin/busybox")
    );

    // Negative control on the same fixture: a symlink whose target the
    // projection never introduces is missing.
    projection
        .insert("/bin/bash", symlink_node("absent"))
        .unwrap();
    assert_eq!(
        projection.resolve_executable("/bin/bash").unwrap(),
        ProjectedExecutable::Missing
    );
}

#[test]
fn overlay_ancestor_symlink_reaches_an_overlay_executable() {
    let root = tempfile::tempdir().unwrap();
    let mut projection = SelectedRootProjection::new(root.path());
    projection.insert("/bin", symlink_node("usr/bin")).unwrap();
    projection.insert("/usr/bin/sh", executable()).unwrap();

    assert_eq!(
        projection.resolve_executable("/bin/sh").unwrap(),
        executable_outcome("/usr/bin/sh")
    );
}

#[test]
fn non_executable_regular_file_is_not_executable() {
    let root = tempfile::tempdir().unwrap();
    write_regular(root.path(), "usr/share/data", 0o644);
    write_executable(root.path(), "usr/bin/run");
    let projection = SelectedRootProjection::new(root.path());

    assert_eq!(
        projection.resolve_executable("/usr/share/data").unwrap(),
        not_executable_outcome("/usr/share/data")
    );

    // Positive control on the same fixture: the same node shape with an
    // execute bit resolves.
    assert_eq!(
        projection.resolve_executable("/usr/bin/run").unwrap(),
        executable_outcome("/usr/bin/run")
    );
}

#[test]
fn directory_is_not_executable() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("usr/bin")).unwrap();
    write_executable(root.path(), "usr/bin/sh");
    let projection = SelectedRootProjection::new(root.path());

    assert_eq!(
        projection.resolve_executable("/usr/bin").unwrap(),
        not_executable_outcome("/usr/bin")
    );

    // Positive control on the same fixture: a child executable resolves.
    assert_eq!(
        projection.resolve_executable("/usr/bin/sh").unwrap(),
        executable_outcome("/usr/bin/sh")
    );
}

#[test]
fn other_nodes_are_never_executable() {
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("run/service.sock");
    fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    write_executable(root.path(), "usr/bin/run");

    let mut projection = SelectedRootProjection::new(root.path());
    projection
        .insert("/run/pipe", ProjectedNode::Other)
        .unwrap();

    assert_eq!(
        projection.resolve_executable("/run/pipe").unwrap(),
        not_executable_outcome("/run/pipe")
    );
    assert_eq!(
        projection.resolve_executable("/run/service.sock").unwrap(),
        not_executable_outcome("/run/service.sock")
    );

    // Positive control on the same fixture: a regular executable resolves.
    assert_eq!(
        projection.resolve_executable("/usr/bin/run").unwrap(),
        executable_outcome("/usr/bin/run")
    );
}

#[test]
fn overlay_directory_node_is_not_executable() {
    let root = tempfile::tempdir().unwrap();
    let mut projection = SelectedRootProjection::new(root.path());
    projection
        .insert("/opt/bin", ProjectedNode::Directory)
        .unwrap();

    assert_eq!(
        projection.resolve_executable("/opt/bin").unwrap(),
        not_executable_outcome("/opt/bin")
    );

    // Positive control on the same implied parent: an introduced executable
    // under the directory resolves.
    projection.insert("/opt/bin/sh", executable()).unwrap();
    assert_eq!(
        projection.resolve_executable("/opt/bin/sh").unwrap(),
        executable_outcome("/opt/bin/sh")
    );
}
