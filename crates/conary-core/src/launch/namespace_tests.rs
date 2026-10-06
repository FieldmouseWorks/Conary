// crates/conary-core/src/launch/namespace_tests.rs

use std::ffi::OsStr;

use nix::errno::Errno;

use super::error::{
    EXIT_LAUNCHER_FAILURE, EXIT_TARGET_NOT_EXECUTABLE, EXIT_TARGET_NOT_FOUND, LaunchError,
    MountStep, ProbeStage,
};
use super::namespace::classify_probe_failure;
use super::search_exhausted;

#[test]
fn denied_identity_map_is_capability_confinement() {
    for errno in [Errno::EPERM, Errno::EACCES] {
        assert!(matches!(
            classify_probe_failure(ProbeStage::IdentityMap, errno, "uid_map"),
            LaunchError::NamespaceCapabilitiesDenied {
                stage: ProbeStage::IdentityMap,
                errno: denied,
            } if denied == errno
        ));
    }
}

#[test]
fn denied_first_mount_is_capability_confinement() {
    assert!(matches!(
        classify_probe_failure(ProbeStage::MountProbe, Errno::EPERM, ""),
        LaunchError::NamespaceCapabilitiesDenied {
            stage: ProbeStage::MountProbe,
            ..
        }
    ));
}

#[test]
fn other_probe_failures_are_not_reported_as_confinement() {
    assert!(matches!(
        classify_probe_failure(ProbeStage::IdentityMap, Errno::EINVAL, "gid_map"),
        LaunchError::IdentityMapFailed {
            file: "gid_map",
            errno: Errno::EINVAL
        }
    ));
    assert!(matches!(
        classify_probe_failure(ProbeStage::MountProbe, Errno::EINVAL, ""),
        LaunchError::MountSetup {
            step: MountStep::MakePrivate,
            errno: Errno::EINVAL,
            ..
        }
    ));
}

#[test]
fn exhausted_search_reports_not_found_or_not_executable() {
    let not_found = search_exhausted(OsStr::new("tree"), false);
    assert!(matches!(not_found, LaunchError::TargetNotFound { ref command } if command == "tree"));
    assert_eq!(not_found.exit_code(), EXIT_TARGET_NOT_FOUND);

    let denied = search_exhausted(OsStr::new("tree"), true);
    assert!(
        matches!(denied, LaunchError::TargetNotExecutable { ref command } if command == "tree")
    );
    assert_eq!(denied.exit_code(), EXIT_TARGET_NOT_EXECUTABLE);
}

#[test]
fn refusals_exit_with_the_launcher_status() {
    let refusal = LaunchError::TreeMissing {
        path: "/nonexistent".into(),
    };
    assert_eq!(refusal.exit_code(), EXIT_LAUNCHER_FAILURE);
    assert_eq!(refusal.kind(), "tree_missing");
    assert_eq!(
        LaunchError::NamespaceCapabilitiesDenied {
            stage: ProbeStage::IdentityMap,
            errno: Errno::EPERM
        }
        .exit_code(),
        EXIT_LAUNCHER_FAILURE
    );
}
