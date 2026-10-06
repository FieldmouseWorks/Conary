// crates/conary-core/src/launch/prepare/tests.rs

use super::*;
use crate::launch::policy::LauncherMountPoint;

fn complete_tree() -> tempfile::TempDir {
    let tree = tempfile::tempdir().unwrap();
    for mount_point in LauncherMountPoint::ALL {
        std::fs::create_dir_all(tree.path().join(mount_point.relative())).unwrap();
    }
    tree
}

#[test]
fn complete_tree_passes_the_checks() {
    let tree = complete_tree();
    let fd = open_launch_tree(tree.path()).unwrap();
    check_launch_tree(tree.path(), &fd).unwrap();
}

#[test]
fn missing_tree_is_refused() {
    let parent = tempfile::tempdir().unwrap();
    let missing = parent.path().join("generations/7/tree");
    assert!(matches!(
        open_launch_tree(&missing),
        Err(LaunchError::TreeMissing { path }) if path == missing
    ));
}

#[test]
fn file_in_place_of_the_tree_is_refused() {
    let parent = tempfile::tempdir().unwrap();
    let file = parent.path().join("tree");
    std::fs::write(&file, b"not a tree").unwrap();
    assert!(matches!(
        open_launch_tree(&file),
        Err(LaunchError::TreeNotDirectory { path }) if path == file
    ));
}

#[test]
fn tree_without_a_launcher_mount_point_is_refused() {
    for missing in LauncherMountPoint::ALL {
        let tree = complete_tree();
        std::fs::remove_dir(tree.path().join(missing.relative())).unwrap();
        let fd = open_launch_tree(tree.path()).unwrap();
        let error = check_launch_tree(tree.path(), &fd).unwrap_err();
        assert!(
            matches!(
                error,
                LaunchError::TreeMissingMountPoint { mount_point, .. } if mount_point == missing
            ),
            "{missing}: {error:?}"
        );
    }
}

#[test]
fn absolute_symlinked_mount_point_resolves_inside_the_tree() {
    // /run -> /var/run must be judged inside the tree, never on the host.
    let tree = complete_tree();
    std::fs::remove_dir(tree.path().join("run")).unwrap();
    std::os::unix::fs::symlink("/var/run", tree.path().join("run")).unwrap();
    let fd = open_launch_tree(tree.path()).unwrap();
    assert!(matches!(
        check_launch_tree(tree.path(), &fd),
        Err(LaunchError::TreeMissingMountPoint {
            mount_point: LauncherMountPoint::Run,
            ..
        })
    ));

    std::fs::create_dir_all(tree.path().join("var/run")).unwrap();
    check_launch_tree(tree.path(), &fd).unwrap();
}

#[test]
fn noexec_mount_flag_is_refused() {
    let tree = Path::new("/var/lib/conary/roots/arch/generations/1/tree");
    assert!(matches!(
        refuse_noexec(tree, FsFlags::ST_NOEXEC | FsFlags::ST_NOSUID),
        Err(LaunchError::TreeOnNoexecMount { path }) if path == tree
    ));
    refuse_noexec(tree, FsFlags::ST_NOSUID | FsFlags::ST_NODEV).unwrap();
}
