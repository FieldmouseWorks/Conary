// crates/conary-core/src/bootstrap/final_system/tests.rs

use super::*;
use crate::bootstrap::stages::StageManager;

fn workspace_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .find(|dir| dir.join("recipes/system").is_dir())
        .expect("workspace root not found from crate manifest ancestors")
}

#[test]
fn test_system_build_order_count() {
    assert_eq!(SYSTEM_BUILD_ORDER.len(), 83);
}

#[test]
fn test_system_build_order_starts_with_man_pages() {
    assert_eq!(SYSTEM_BUILD_ORDER[0], "man-pages");
}

#[test]
fn test_system_build_order_ends_with_linux() {
    assert_eq!(SYSTEM_BUILD_ORDER[82], "linux");
}

#[test]
fn test_system_build_order_includes_sqlite_before_python() {
    let sqlite_idx = SYSTEM_BUILD_ORDER
        .iter()
        .position(|pkg| *pkg == "sqlite")
        .expect("sqlite in system build order");
    let python_idx = SYSTEM_BUILD_ORDER
        .iter()
        .position(|pkg| *pkg == "python")
        .expect("python in system build order");

    assert!(sqlite_idx < python_idx);
}

#[test]
fn test_system_build_order_includes_pyelftools_before_systemd() {
    let pyelftools_idx = SYSTEM_BUILD_ORDER
        .iter()
        .position(|pkg| *pkg == "pyelftools")
        .expect("pyelftools in system build order");
    let systemd_idx = SYSTEM_BUILD_ORDER
        .iter()
        .position(|pkg| *pkg == "systemd")
        .expect("systemd in system build order");

    assert!(pyelftools_idx < systemd_idx);
}

#[test]
fn test_system_build_order_includes_composefs_after_meson_before_kmod() {
    let composefs_idx = SYSTEM_BUILD_ORDER
        .iter()
        .position(|pkg| *pkg == "composefs")
        .expect("composefs in system build order");
    let meson_idx = SYSTEM_BUILD_ORDER
        .iter()
        .position(|pkg| *pkg == "meson")
        .expect("meson in system build order");
    let kmod_idx = SYSTEM_BUILD_ORDER
        .iter()
        .position(|pkg| *pkg == "kmod")
        .expect("kmod in system build order");

    assert!(meson_idx < composefs_idx);
    assert!(composefs_idx < kmod_idx);
}

#[test]
fn test_system_build_order_includes_linux_kernel() {
    assert!(
        SYSTEM_BUILD_ORDER.contains(&"linux"),
        "Phase 5 image generation requires the kernel recipe to run during bootstrap"
    );
}

#[test]
fn test_system_build_order_tracks_lfs13_selection_with_conary_bootloader_deviation() {
    assert!(SYSTEM_BUILD_ORDER.contains(&"lz4"));
    assert!(SYSTEM_BUILD_ORDER.contains(&"pcre2"));
    assert!(SYSTEM_BUILD_ORDER.contains(&"packaging"));
    assert!(SYSTEM_BUILD_ORDER.contains(&"elfutils"));
    assert!(SYSTEM_BUILD_ORDER.contains(&"pyelftools"));
    assert!(SYSTEM_BUILD_ORDER.contains(&"linux"));
    assert!(!SYSTEM_BUILD_ORDER.contains(&"check"));
    assert!(!SYSTEM_BUILD_ORDER.contains(&"grub"));
}

#[test]
fn test_system_build_order_has_recipe_files() {
    for pkg in SYSTEM_BUILD_ORDER {
        let filename = FinalSystemBuilder::recipe_filename(pkg);
        let recipe_path = workspace_root()
            .join("recipes/system")
            .join(format!("{filename}.toml"));
        assert!(
            recipe_path.is_file(),
            "missing Phase 3 recipe file for {pkg}: {}",
            recipe_path.display()
        );
    }
}

#[test]
fn test_new_requires_usr_bin() {
    let work = tempfile::tempdir().unwrap();
    let lfs = tempfile::tempdir().unwrap();
    let config = BootstrapConfig::new();

    let result = FinalSystemBuilder::new(work.path(), lfs.path(), config);
    assert!(result.is_err());
}

#[test]
fn test_new_succeeds_with_usr_bin() {
    let work = tempfile::tempdir().unwrap();
    let lfs = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(lfs.path().join("usr/bin")).unwrap();

    let config = BootstrapConfig::new();

    let builder = FinalSystemBuilder::new(work.path(), lfs.path(), config);
    assert!(builder.is_ok());
}

#[test]
fn test_build_all_checkpoints_completed_packages() {
    let work = tempfile::tempdir().unwrap();
    let lfs = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(lfs.path().join("usr/bin")).unwrap();

    let config = BootstrapConfig::new();

    let mut sm = StageManager::new(work.path()).unwrap();
    let mut builder = FinalSystemBuilder::new(work.path(), lfs.path(), config).unwrap();
    let mut requested = Vec::new();
    builder
        .build_all_with(&[], &mut sm, |current, package| {
            assert_eq!(current.completed(), requested);
            assert_eq!(
                StageManager::new(work.path())
                    .unwrap()
                    .completed_packages(BootstrapStage::FinalSystem),
                requested
            );
            requested.push(package.to_string());
            Ok(())
        })
        .unwrap();
    assert_eq!(requested, SYSTEM_BUILD_ORDER);
    assert_eq!(builder.completed(), requested);
    assert_eq!(
        StageManager::new(work.path())
            .unwrap()
            .completed_packages(BootstrapStage::FinalSystem),
        requested
    );
}

#[test]
fn test_build_from_resume() {
    let work = tempfile::tempdir().unwrap();
    let lfs = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(lfs.path().join("usr/bin")).unwrap();

    let config = BootstrapConfig::new();

    let mut sm = StageManager::new(work.path()).unwrap();
    let mut builder = FinalSystemBuilder::new(work.path(), lfs.path(), config).unwrap();
    let mut requested = Vec::new();
    builder
        .build_from_with("gcc", &mut sm, |_, package| {
            requested.push(package.to_string());
            Ok(())
        })
        .unwrap();
    // gcc is the inclusive resume point at index 27.
    assert_eq!(requested, SYSTEM_BUILD_ORDER[27..]);
    assert_eq!(builder.completed(), requested);
    assert_eq!(
        StageManager::new(work.path())
            .unwrap()
            .completed_packages(BootstrapStage::FinalSystem),
        requested
    );
}

#[test]
fn test_build_from_invalid_package() {
    let work = tempfile::tempdir().unwrap();
    let lfs = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(lfs.path().join("usr/bin")).unwrap();

    let config = BootstrapConfig::new();

    let mut sm = StageManager::new(work.path()).unwrap();
    let mut builder = FinalSystemBuilder::new(work.path(), lfs.path(), config).unwrap();
    let result = builder.build_from("nonexistent-package", &mut sm);
    assert!(result.is_err());
}

#[test]
fn test_prepare_chroot_build_dirs_uses_sysroot_staging_area() {
    let work = tempfile::tempdir().unwrap();
    let lfs = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(lfs.path().join("usr/bin")).unwrap();

    let config = BootstrapConfig::new();

    let builder = FinalSystemBuilder::new(work.path(), lfs.path(), config).unwrap();
    let (src_dir, build_dir) = builder.prepare_chroot_build_dirs("man-pages").unwrap();

    assert_eq!(
        src_dir,
        lfs.path()
            .join("var/tmp/conary-bootstrap/final-system/man-pages/src")
    );
    assert_eq!(
        build_dir,
        lfs.path()
            .join("var/tmp/conary-bootstrap/final-system/man-pages/build")
    );
}

#[test]
fn test_path_in_chroot_rewrites_sysroot_staging_paths() {
    let work = tempfile::tempdir().unwrap();
    let lfs = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(lfs.path().join("usr/bin")).unwrap();

    let config = BootstrapConfig::new();

    let builder = FinalSystemBuilder::new(work.path(), lfs.path(), config).unwrap();
    let staged_src = lfs
        .path()
        .join("var/tmp/conary-bootstrap/final-system/man-pages/src");

    assert_eq!(
        builder.path_in_chroot(&staged_src).unwrap(),
        "/var/tmp/conary-bootstrap/final-system/man-pages/src"
    );
}

#[test]
fn test_setup_chroot_creates_virtual_fs_directories() {
    let work = tempfile::tempdir().unwrap();
    let lfs = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(lfs.path().join("usr/bin")).unwrap();

    let config = BootstrapConfig::new();

    let builder = FinalSystemBuilder::new(work.path(), lfs.path(), config).unwrap();
    let _ = builder.setup_chroot();

    assert!(lfs.path().join("dev").exists());
    assert!(lfs.path().join("proc").exists());
    assert!(lfs.path().join("sys").exists());
    assert!(lfs.path().join("run").exists());
}

#[test]
fn test_setup_chroot_repairs_missing_shadow_prerequisite_groups() {
    let work = tempfile::tempdir().unwrap();
    let lfs = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(lfs.path().join("usr/bin")).unwrap();
    std::fs::create_dir_all(lfs.path().join("etc")).unwrap();
    std::fs::write(
        lfs.path().join("etc/group"),
        "root:x:0:\nwheel:x:10:\ntty:x:5:\nnogroup:x:65534:\n",
    )
    .unwrap();

    let config = BootstrapConfig::new();

    let builder = FinalSystemBuilder::new(work.path(), lfs.path(), config).unwrap();
    let _ = builder.setup_chroot();

    let group = std::fs::read_to_string(lfs.path().join("etc/group")).unwrap();
    assert!(group.contains("mail:x:34:"));
    assert!(group.contains("users:x:999:"));
    assert!(group.contains("wheel:x:10:"));
}

#[test]
fn test_build_all_skips_completed_and_does_not_checkpoint_failed_package() {
    let work = tempfile::tempdir().unwrap();
    let lfs = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(lfs.path().join("usr/bin")).unwrap();
    let mut stages = StageManager::new(work.path()).unwrap();
    stages
        .mark_package_complete(BootstrapStage::FinalSystem, "man-pages")
        .unwrap();
    let mut builder =
        FinalSystemBuilder::new(work.path(), lfs.path(), BootstrapConfig::new()).unwrap();
    let mut attempted = Vec::new();
    let error = builder
        .build_all_with(&["man-pages".to_string()], &mut stages, |_, package| {
            attempted.push(package.to_string());
            if package == "glibc" {
                return Err(FinalSystemError::BuildFailed {
                    package: package.to_string(),
                    reason: "injected".to_string(),
                });
            }
            Ok(())
        })
        .unwrap_err();
    assert!(
        matches!(error, FinalSystemError::BuildFailed { package, reason } if package == "glibc" && reason == "injected")
    );
    assert_eq!(attempted, ["iana-etc", "glibc"]);
    assert_eq!(builder.completed(), ["iana-etc"]);
    assert_eq!(
        StageManager::new(work.path())
            .unwrap()
            .completed_packages(BootstrapStage::FinalSystem),
        ["man-pages", "iana-etc"]
    );
}
