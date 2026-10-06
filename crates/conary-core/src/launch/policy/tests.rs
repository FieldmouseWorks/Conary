// crates/conary-core/src/launch/policy/tests.rs

use super::*;
use crate::launch::error::{LaunchError, UsernsSetting};

fn bound_home(declared: &str, resolved: &str) -> HomeBind {
    decide_home(Some((
        PathBuf::from(declared),
        Some(PathBuf::from(resolved)),
    )))
}

#[test]
fn sysctl_values_parse_with_trailing_newline() {
    assert_eq!(parse_sysctl_value("1\n"), Some(1));
    assert_eq!(parse_sysctl_value(" 235922 \n"), Some(235_922));
    assert_eq!(parse_sysctl_value("yes"), None);
    assert_eq!(parse_sysctl_value(""), None);
}

#[test]
fn enabled_sysctls_do_not_refuse() {
    let enabled = UsernsSysctls {
        unprivileged_userns_clone: Some(1),
        max_user_namespaces: Some(63_000),
    };
    assert!(enabled.refusal(1000).is_none());
    // Absent settings never refuse: unshare stays the authority.
    assert!(UsernsSysctls::default().refusal(1000).is_none());
}

#[test]
fn max_user_namespaces_zero_refuses_everyone() {
    let disabled = UsernsSysctls {
        unprivileged_userns_clone: Some(1),
        max_user_namespaces: Some(0),
    };
    for euid in [0, 1000] {
        assert!(matches!(
            disabled.refusal(euid),
            Some(LaunchError::UserNamespacesDisabled {
                setting: UsernsSetting::MaxUserNamespaces
            })
        ));
    }
}

#[test]
fn unprivileged_userns_clone_zero_refuses_only_unprivileged_callers() {
    let disabled = UsernsSysctls {
        unprivileged_userns_clone: Some(0),
        max_user_namespaces: Some(63_000),
    };
    assert!(matches!(
        disabled.refusal(1000),
        Some(LaunchError::UserNamespacesDisabled {
            setting: UsernsSetting::UnprivilegedUsernsClone
        })
    ));
    assert!(disabled.refusal(0).is_none());
}

#[test]
fn sysctls_are_read_from_a_proc_sys_tree() {
    let proc_sys = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(proc_sys.path().join("kernel")).unwrap();
    std::fs::create_dir_all(proc_sys.path().join("user")).unwrap();
    std::fs::write(
        proc_sys.path().join("kernel/unprivileged_userns_clone"),
        "0\n",
    )
    .unwrap();
    std::fs::write(proc_sys.path().join("user/max_user_namespaces"), "100\n").unwrap();
    let read = UsernsSysctls::read(proc_sys.path());
    assert_eq!(read.unprivileged_userns_clone, Some(0));
    assert_eq!(read.max_user_namespaces, Some(100));

    // A kernel without the Debian sysctl reports it as absent.
    std::fs::remove_file(proc_sys.path().join("kernel/unprivileged_userns_clone")).unwrap();
    assert_eq!(
        UsernsSysctls::read(proc_sys.path()).unprivileged_userns_clone,
        None
    );
}

#[test]
fn ordinary_home_is_bound_at_its_declared_path() {
    let home = bound_home("/home/alice", "/var/home/alice");
    let HomeBind::Bound(home) = home else {
        panic!("ordinary home must be bound: {home:?}")
    };
    assert_eq!(home.declared, Path::new("/home/alice"));
    assert_eq!(home.resolved, Path::new("/var/home/alice"));
    assert!(matches!(bound_home("/root", "/root"), HomeBind::Bound(_)));
}

#[test]
fn homes_that_collide_with_launcher_mounts_are_skipped() {
    for (declared, resolved) in [
        ("/", "/"),
        ("/tmp/alice", "/tmp/alice"),
        ("/run/alice", "/run/alice"),
        ("/var", "/var"),
        ("/home/alice", "/tmp/alice"),
    ] {
        assert!(
            matches!(
                bound_home(declared, resolved),
                HomeBind::Skipped(HomeSkip::OverlapsLauncherMount { .. })
            ),
            "{declared} -> {resolved}"
        );
    }
}

#[test]
fn missing_or_malformed_homes_are_skipped_with_their_reason() {
    assert_eq!(
        decide_home(None),
        HomeBind::Skipped(HomeSkip::NoPasswdEntry)
    );
    assert_eq!(
        decide_home(Some((PathBuf::from("/home/gone"), None))),
        HomeBind::Skipped(HomeSkip::Missing(PathBuf::from("/home/gone")))
    );
    assert_eq!(
        decide_home(Some((PathBuf::from("home/rel"), Some(PathBuf::from("/x"))))),
        HomeBind::Skipped(HomeSkip::NotAbsolute(PathBuf::from("home/rel")))
    );
}

#[test]
fn host_bind_plan_has_the_typed_binds_in_order() {
    let home = bound_home("/home/alice", "/var/home/alice");
    let plan = host_bind_plan(1000, &home);
    let kinds: Vec<HostBindKind> = plan.iter().map(|bind| bind.kind).collect();
    let mut expected = vec![
        HostBindKind::Proc,
        HostBindKind::Dev,
        HostBindKind::Sys,
        HostBindKind::Tmp,
        HostBindKind::VarTmp,
        HostBindKind::Home,
        HostBindKind::RuntimeDir,
        HostBindKind::HostRoot,
    ];
    expected.extend(HostEtcFile::ALL.map(HostBindKind::Etc));
    assert_eq!(kinds, expected);

    let find = |kind| plan.iter().find(|bind| bind.kind == kind).unwrap();
    let host_root = find(HostBindKind::HostRoot);
    assert_eq!(host_root.source, Path::new("/"));
    assert_eq!(host_root.target, Path::new("/run/host"));
    assert!(host_root.readonly && host_root.recursive);
    assert!(find(HostBindKind::Sys).readonly);
    assert!(!find(HostBindKind::Tmp).readonly);
    assert!(!find(HostBindKind::Proc).readonly);

    let home_bind = find(HostBindKind::Home);
    assert_eq!(home_bind.source, Path::new("/var/home/alice"));
    assert_eq!(home_bind.target, Path::new("/home/alice"));

    let runtime = find(HostBindKind::RuntimeDir);
    assert_eq!(runtime.target, Path::new("/run/user/1000"));
    assert_eq!(runtime.requirement, Requirement::Optional);

    let resolv = find(HostBindKind::Etc(HostEtcFile::ResolvConf));
    assert_eq!(resolv.target, Path::new("/etc/resolv.conf"));
    assert!(resolv.readonly && !resolv.recursive);
    assert_eq!(resolv.requirement, Requirement::Optional);
}

#[test]
fn skipped_home_is_absent_from_the_plan() {
    let plan = host_bind_plan(1000, &HomeBind::Skipped(HomeSkip::NoPasswdEntry));
    assert!(plan.iter().all(|bind| bind.kind != HostBindKind::Home));
    assert!(plan.iter().any(|bind| bind.kind == HostBindKind::Tmp));
}

#[test]
fn working_directory_in_a_bound_directory_is_kept() {
    let home = bound_home("/home/alice", "/home/alice");
    let bound = bound_directories(1000, &home);
    for cwd in [
        "/home/alice",
        "/home/alice/src/project",
        "/tmp",
        "/tmp/build",
        "/var/tmp/x",
        "/run/user/1000/app",
        "/run/host/srv",
    ] {
        assert_eq!(
            map_working_directory(Path::new(cwd), &bound).unwrap(),
            Path::new(cwd)
        );
    }
}

#[test]
fn working_directory_under_a_resolved_home_maps_to_the_declared_home() {
    let home = bound_home("/home/alice", "/var/home/alice");
    let bound = bound_directories(1000, &home);
    assert_eq!(
        map_working_directory(Path::new("/var/home/alice/src"), &bound).unwrap(),
        Path::new("/home/alice/src")
    );
}

#[test]
fn working_directory_outside_the_bound_set_is_refused_with_its_host_view() {
    let home = bound_home("/home/alice", "/home/alice");
    let bound = bound_directories(1000, &home);
    for cwd in ["/", "/srv/data", "/home/bob", "/run/user/1001", "/tmpfoo"] {
        let error = map_working_directory(Path::new(cwd), &bound).unwrap_err();
        let LaunchError::WorkingDirectoryOutsideBoundSet {
            cwd: refused,
            host_view,
        } = error
        else {
            panic!("{cwd}: unexpected {error:?}")
        };
        assert_eq!(refused, Path::new(cwd));
        assert_eq!(
            host_view,
            Path::new("/run/host").join(cwd.trim_start_matches('/'))
        );
    }
}

#[test]
fn exec_candidates_follow_execvp_search_order() {
    assert_eq!(
        exec_candidates(
            OsStr::new("tree"),
            Some(OsStr::new("/usr/local/bin::/usr/bin"))
        ),
        vec![
            PathBuf::from("/usr/local/bin/tree"),
            PathBuf::from("./tree"),
            PathBuf::from("/usr/bin/tree"),
        ]
    );
    assert_eq!(
        exec_candidates(OsStr::new("./run.sh"), Some(OsStr::new("/usr/bin"))),
        vec![PathBuf::from("./run.sh")]
    );
    assert_eq!(
        exec_candidates(OsStr::new("sh"), None),
        vec![PathBuf::from("/bin/sh"), PathBuf::from("/usr/bin/sh")]
    );
}
