// crates/conary-core/src/generation/root_manifest/overlay/scratch/tests.rs

#![cfg(test)]

use super::*;
use crate::generation::root_manifest::{
    OverlayHardlinkCopyUp, OverlayLowerDirectoryRename, OverlayMetadataCopyUp,
    OverlayOpaqueDirectory, OverlayWhiteoutEncoding, SELECTED_ROOT_OVERLAY_CAPABILITIES_VERSION,
};
use std::cell::RefCell;
use std::rc::Rc;

/// A scratch mount handle that records unmounts instead of touching the host.
struct FakeScratchMount {
    path: PathBuf,
    unmounts: Rc<RefCell<Vec<PathBuf>>>,
}

impl ScratchMount for FakeScratchMount {
    fn path(&self) -> &Path {
        &self.path
    }

    fn unmount(self) -> crate::Result<()> {
        let FakeScratchMount { path, unmounts } = self;
        unmounts.borrow_mut().push(path);
        Ok(())
    }
}

fn capabilities() -> SelectedRootOverlayCapabilities {
    SelectedRootOverlayCapabilities {
        version: SELECTED_ROOT_OVERLAY_CAPABILITIES_VERSION,
        profile: SelectedRootOverlayProfile::trusted(),
        whiteout_encoding: OverlayWhiteoutEncoding::CharacterDeviceZeroZero,
        opaque_directory: OverlayOpaqueDirectory::LogicalY,
        hardlink_copy_up: OverlayHardlinkCopyUp::Preserved,
        lower_directory_rename: OverlayLowerDirectoryRename::CrossDevice,
        metadata_copy_up: OverlayMetadataCopyUp::CompleteData,
        scratch_placement: OverlayScratchPlacement::SessionDirectory,
    }
}

#[test]
fn session_directory_is_selected_when_its_probe_passes() {
    let workspace = tempfile::tempdir().unwrap();
    let session_dir = workspace.path().join("session");
    let probe_calls = Rc::new(RefCell::new(0_u32));
    let observed_probes = Rc::clone(&probe_calls);
    let mounts = Rc::new(RefCell::new(Vec::<PathBuf>::new()));
    let observed_mounts = Rc::clone(&mounts);

    let selection = select_with(
        &session_dir,
        &SelectedRootOverlayProfile::trusted(),
        |_path, _profile| {
            *observed_probes.borrow_mut() += 1;
            Ok(capabilities())
        },
        move |path| -> crate::Result<FakeScratchMount> {
            observed_mounts.borrow_mut().push(path.to_path_buf());
            Ok(FakeScratchMount {
                path: path.to_path_buf(),
                unmounts: Rc::new(RefCell::new(Vec::new())),
            })
        },
    )
    .unwrap();

    assert_eq!(
        selection.capabilities.scratch_placement,
        OverlayScratchPlacement::SessionDirectory
    );
    assert_eq!(selection.directory, session_dir);
    assert!(selection.tmpfs.is_none());
    assert_eq!(*probe_calls.borrow(), 1);
    assert!(
        mounts.borrow().is_empty(),
        "tmpfs mount must not be attempted"
    );
}

#[test]
fn private_tmpfs_is_selected_when_session_directory_fails() {
    let workspace = tempfile::tempdir().unwrap();
    let session_dir = workspace.path().join("session");
    let scratch = session_dir.join("scratch");
    let unmounts = Rc::new(RefCell::new(Vec::<PathBuf>::new()));
    let observed_unmounts = Rc::clone(&unmounts);

    let selection = select_with(
        &session_dir,
        &SelectedRootOverlayProfile::trusted(),
        |path, _profile| {
            if path == session_dir.as_path() {
                Err(crate::Error::IoError(
                    "session directory rejects an OverlayFS upper".to_string(),
                ))
            } else {
                Ok(capabilities())
            }
        },
        |path| -> crate::Result<FakeScratchMount> {
            Ok(FakeScratchMount {
                path: path.to_path_buf(),
                unmounts: Rc::clone(&observed_unmounts),
            })
        },
    )
    .unwrap();

    assert_eq!(
        selection.capabilities.scratch_placement,
        OverlayScratchPlacement::PrivateTmpfs
    );
    assert_eq!(selection.directory, scratch);
    assert!(selection.tmpfs.is_some());
    assert!(unmounts.borrow().is_empty());
}

#[test]
fn every_failed_candidate_is_reported_in_order() {
    let workspace = tempfile::tempdir().unwrap();
    let session_dir = workspace.path().join("session");
    let unmounts = Rc::new(RefCell::new(Vec::<PathBuf>::new()));
    let observed_unmounts = Rc::clone(&unmounts);

    let result = select_with(
        &session_dir,
        &SelectedRootOverlayProfile::trusted(),
        |_path, _profile| Err(crate::Error::IoError("probe refused".to_string())),
        |path| -> crate::Result<FakeScratchMount> {
            Ok(FakeScratchMount {
                path: path.to_path_buf(),
                unmounts: Rc::clone(&observed_unmounts),
            })
        },
    );

    match result {
        Err(crate::Error::SelectedRootOverlayUnsupported(failures)) => {
            let placements = failures
                .iter()
                .map(|failure| failure.placement)
                .collect::<Vec<_>>();
            assert_eq!(
                placements,
                vec![
                    OverlayScratchPlacement::SessionDirectory,
                    OverlayScratchPlacement::PrivateTmpfs,
                ]
            );
        }
        _ => panic!("expected SelectedRootOverlayUnsupported"),
    }
    assert_eq!(
        &*unmounts.borrow(),
        &[session_dir.join("scratch")],
        "a mounted tmpfs whose probe failed must be unmounted"
    );
}

#[test]
fn tmpfs_mount_failure_is_a_candidate_failure() {
    let workspace = tempfile::tempdir().unwrap();
    let session_dir = workspace.path().join("session");

    let result = select_with(
        &session_dir,
        &SelectedRootOverlayProfile::trusted(),
        |_path, _profile| Err(crate::Error::IoError("session probe refused".to_string())),
        |_path| -> crate::Result<FakeScratchMount> {
            Err(crate::Error::IoError("tmpfs mount refused".to_string()))
        },
    );

    match result {
        Err(crate::Error::SelectedRootOverlayUnsupported(failures)) => {
            let placements = failures
                .iter()
                .map(|failure| failure.placement)
                .collect::<Vec<_>>();
            assert_eq!(
                placements,
                vec![
                    OverlayScratchPlacement::SessionDirectory,
                    OverlayScratchPlacement::PrivateTmpfs,
                ]
            );
        }
        _ => panic!("expected SelectedRootOverlayUnsupported"),
    }
}

#[test]
#[ignore = "requires CAP_SYS_ADMIN, OverlayFS, and tmpfs"]
fn private_tmpfs_scratch_passes_the_functional_probe() {
    let workspace = tempfile::tempdir().unwrap();
    let tmpfs = MountedScratchTmpfs::mount(&workspace.path().join("scratch")).unwrap();

    probe_selected_root_overlay_profile(tmpfs.path(), &SelectedRootOverlayProfile::trusted())
        .expect("a private tmpfs must host a selected-root OverlayFS upper");

    tmpfs.unmount().unwrap();
}

#[test]
#[ignore = "requires CAP_SYS_ADMIN, OverlayFS, and tmpfs"]
fn selection_on_overlayfs_workspace_falls_back_to_private_tmpfs() {
    let workspace = tempfile::tempdir().unwrap();
    let lower = workspace.path().join("lower");
    let upper = workspace.path().join("upper");
    let work = workspace.path().join("work");
    let merged = workspace.path().join("merged");
    for directory in [&lower, &upper, &work, &merged] {
        fs::create_dir(directory).unwrap();
    }
    let options = format!(
        "lowerdir={},upperdir={},workdir={}",
        lower.display(),
        upper.display(),
        work.display()
    );
    mount(
        Some("overlay"),
        &merged,
        Some("overlay"),
        MsFlags::empty(),
        Some(options.as_str()),
    )
    .unwrap();

    let selection = select_selected_root_overlay_scratch(
        &merged.join("session"),
        &SelectedRootOverlayProfile::trusted(),
    )
    .unwrap();
    assert_eq!(selection.placement(), OverlayScratchPlacement::PrivateTmpfs);

    let (_, _, tmpfs) = selection.into_parts();
    tmpfs
        .expect("private tmpfs scratch must be owned by the selection")
        .unmount()
        .unwrap();
    umount2(&merged, MntFlags::empty()).unwrap();
}
