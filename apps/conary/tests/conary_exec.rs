// apps/conary/tests/conary_exec.rs
//! End-to-end proof of the `conary-exec` launcher against a hand-built,
//! busybox-only launch tree.
//!
//! Tests that need user namespaces ask `conary-exec --check` first. When the
//! host refuses (for example Ubuntu's AppArmor userns restriction without the
//! conary-exec profile loaded) they skip and print the typed refusal kind,
//! unless `CONARY_EXEC_REQUIRE_USERNS=1`, which turns a refusal into a failure
//! for lanes that must prove the launcher.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const LAUNCHER: &str = env!("CARGO_BIN_EXE_conary-exec");
const BUSYBOX: &str = "/usr/bin/busybox";
const APPLETS: &[&str] = &["sh", "cat", "echo", "id", "pwd", "touch", "test"];
/// A host file under /usr that the busybox tree never contains.
const HOST_USR_MARKER: &str = "/usr/bin/env";
const TREE_MARKER: &str = "conary-exec-tree-marker";

/// ELF64 little-endian: does the file request a program interpreter?
/// A dynamically linked busybox would need the host's loader in the tree.
fn elf_has_interpreter(bytes: &[u8]) -> Option<bool> {
    if bytes.len() < 64 || &bytes[..4] != b"\x7fELF" || bytes[4] != 2 || bytes[5] != 1 {
        return None;
    }
    let read_u16 = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]) as usize;
    let phoff = u64::from_le_bytes(bytes[32..40].try_into().ok()?) as usize;
    let (phentsize, phnum) = (read_u16(54), read_u16(56));
    const PT_INTERP: u32 = 3;
    Some((0..phnum).any(|index| {
        let at = phoff + index * phentsize;
        bytes
            .get(at..at + 4)
            .map(|kind| u32::from_le_bytes(kind.try_into().unwrap()) == PT_INTERP)
            .unwrap_or(false)
    }))
}

struct Fixture {
    _dir: tempfile::TempDir,
    tree: PathBuf,
}

/// A static busybox tree with every launcher mount point, its own /etc
/// marker, and a deliberately non-executable file.
fn build_tree() -> Option<Fixture> {
    let busybox = fs::read(BUSYBOX).ok()?;
    if elf_has_interpreter(&busybox) != Some(false) {
        eprintln!("skipping: {BUSYBOX} is not a static ELF64 executable");
        return None;
    }
    let dir = tempfile::Builder::new()
        .prefix("conary-exec-tree")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap();
    let tree = dir.path().join("tree");
    for path in [
        "bin", "usr/bin", "proc", "dev", "sys", "tmp", "var/tmp", "etc", "run", "home", "root",
    ] {
        fs::create_dir_all(tree.join(path)).unwrap();
    }
    fs::write(tree.join("bin/busybox"), &busybox).unwrap();
    fs::set_permissions(tree.join("bin/busybox"), fs::Permissions::from_mode(0o755)).unwrap();
    for applet in APPLETS {
        std::os::unix::fs::symlink("busybox", tree.join("bin").join(applet)).unwrap();
    }
    fs::write(tree.join("etc").join(TREE_MARKER), b"tree\n").unwrap();
    fs::write(tree.join("etc/passwd"), b"tree-only:x:0:0::/:/bin/sh\n").unwrap();
    fs::write(tree.join("bin/not-executable"), b"#!/bin/sh\n").unwrap();
    fs::set_permissions(
        tree.join("bin/not-executable"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    Some(Fixture { _dir: dir, tree })
}

fn launcher_ready() -> bool {
    let output = Command::new(LAUNCHER).arg("--check").output().unwrap();
    let report: serde_json::Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("--check printed no JSON report ({error}): {output:?}"));
    if report["ready"] == true {
        assert!(output.status.success(), "{output:?}");
        return true;
    }
    assert_eq!(output.status.code(), Some(125), "{output:?}");
    if std::env::var_os("CONARY_EXEC_REQUIRE_USERNS").is_some_and(|value| value == "1") {
        panic!("conary-exec is required but refused: {report}");
    }
    eprintln!(
        "skipping: conary-exec refused with {}: {}",
        report["kind"], report["message"]
    );
    false
}

/// A ready launcher and a tree, or `None` to skip.
fn ready_fixture() -> Option<Fixture> {
    if !launcher_ready() {
        return None;
    }
    build_tree()
}

fn launch(fixture: &Fixture, cwd: &Path, command: &[&str], stdin: &[u8]) -> Output {
    let mut child = Command::new(LAUNCHER)
        .arg("--tree")
        .arg(&fixture.tree)
        .arg("--")
        .args(command)
        .current_dir(cwd)
        .env("PATH", "/bin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    child.wait_with_output().unwrap()
}

fn tmp_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("conary-exec-cwd")
        .tempdir_in("/tmp")
        .unwrap()
}

fn stdout(output: &Output) -> String {
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout.clone()).unwrap()
}

#[test]
fn exit_status_passes_through() {
    let Some(fixture) = ready_fixture() else {
        return;
    };
    let cwd = tmp_dir();
    let output = launch(&fixture, cwd.path(), &["sh", "-c", "exit 7"], b"");
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    let output = launch(&fixture, cwd.path(), &["sh", "-c", "exit 0"], b"");
    assert_eq!(output.status.code(), Some(0), "{output:?}");
}

#[test]
fn stdio_passes_through() {
    let Some(fixture) = ready_fixture() else {
        return;
    };
    let cwd = tmp_dir();
    let output = launch(&fixture, cwd.path(), &["cat"], b"launch-stdin\n");
    assert_eq!(stdout(&output), "launch-stdin\n");
    let output = launch(
        &fixture,
        cwd.path(),
        &["sh", "-c", "echo to-stderr >&2"],
        b"",
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stderr, b"to-stderr\n");
    assert!(output.stdout.is_empty());
}

#[test]
fn working_directory_is_preserved_under_tmp_and_home() {
    let Some(fixture) = ready_fixture() else {
        return;
    };
    let cwd = tmp_dir();
    let canonical = fs::canonicalize(cwd.path()).unwrap();
    let output = launch(&fixture, &canonical, &["pwd", "-P"], b"");
    assert_eq!(stdout(&output).trim_end(), canonical.to_str().unwrap());

    let uid = nix::unistd::geteuid();
    let home = nix::unistd::User::from_uid(uid).unwrap().unwrap().dir;
    let home_cwd = tempfile::Builder::new()
        .prefix(".conary-exec-cwd")
        .tempdir_in(&home)
        .unwrap();
    let output = launch(&fixture, home_cwd.path(), &["pwd"], b"");
    assert_eq!(
        stdout(&output).trim_end(),
        home_cwd.path().to_str().unwrap()
    );
}

#[test]
fn caller_identity_is_preserved() {
    let Some(fixture) = ready_fixture() else {
        return;
    };
    let cwd = tmp_dir();
    let uid = launch(&fixture, cwd.path(), &["id", "-u"], b"");
    assert_eq!(stdout(&uid).trim_end(), nix::unistd::geteuid().to_string());
    let gid = launch(&fixture, cwd.path(), &["id", "-g"], b"");
    assert_eq!(stdout(&gid).trim_end(), nix::unistd::getegid().to_string());
}

#[test]
fn host_usr_is_invisible_and_host_root_is_at_run_host() {
    let Some(fixture) = ready_fixture() else {
        return;
    };
    assert!(
        Path::new(HOST_USR_MARKER).exists(),
        "host lacks {HOST_USR_MARKER}"
    );
    let cwd = tmp_dir();
    let exists = |path: &str| {
        let output = launch(&fixture, cwd.path(), &["test", "-e", path], b"");
        match output.status.code() {
            Some(0) => true,
            Some(1) => false,
            _ => panic!("test -e {path}: {output:?}"),
        }
    };
    assert!(!exists(HOST_USR_MARKER), "host /usr leaked into the root");
    assert!(exists(&format!("/run/host{HOST_USR_MARKER}")));
    assert!(
        exists(&format!("/etc/{TREE_MARKER}")),
        "tree /etc not mirrored"
    );
    assert!(!exists(&format!("/run/host/etc/{TREE_MARKER}")));

    // Host identity files are bound over the tree's own /etc.
    if let Ok(host_passwd) = fs::read_to_string("/etc/passwd") {
        let output = launch(&fixture, cwd.path(), &["cat", "/etc/passwd"], b"");
        assert_eq!(stdout(&output), host_passwd);
    }
}

/// Per-mount options of the topmost mount at `mount_point`, from
/// `/proc/self/mountinfo`: field 1 is the mount id, 2 its parent, 5 the
/// mount point, 6 the per-mount options. Lines follow mount-id order, not
/// stacking order, so the topmost mount is the one no other mount at the
/// same point is stacked on.
fn mount_options(mountinfo: &str, mount_point: &str) -> Vec<String> {
    let at_point: Vec<(&str, &str, &str)> = mountinfo
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(' ').collect();
            (fields.get(4) == Some(&mount_point)).then(|| (fields[0], fields[1], fields[5]))
        })
        .collect();
    let topmost: Vec<&str> = at_point
        .iter()
        .filter(|(id, _, _)| !at_point.iter().any(|(_, parent, _)| parent == id))
        .map(|(_, _, options)| *options)
        .collect();
    assert_eq!(
        topmost.len(),
        1,
        "expected one topmost mount at {mount_point} in:\n{mountinfo}"
    );
    topmost[0].split(',').map(str::to_string).collect()
}

#[test]
fn tree_and_host_root_are_read_only() {
    let Some(fixture) = ready_fixture() else {
        return;
    };
    let cwd = tmp_dir();
    let mountinfo = stdout(&launch(
        &fixture,
        cwd.path(),
        &["cat", "/proc/self/mountinfo"],
        b"",
    ));
    for read_only in ["/", "/run/host", "/etc", "/sys", "/run"] {
        assert!(
            mount_options(&mountinfo, read_only).contains(&"ro".to_string()),
            "{read_only} must be read-only"
        );
    }
    assert!(mount_options(&mountinfo, "/tmp").contains(&"rw".to_string()));

    // Writes into the tree fail and leave no file behind.
    let refused = launch(&fixture, cwd.path(), &["touch", "/bin/written"], b"");
    assert!(!refused.status.success(), "{refused:?}");
    assert!(!fixture.tree.join("bin/written").exists());
    let refused = launch(&fixture, cwd.path(), &["touch", "/run/host/tmp/x"], b"");
    assert!(!refused.status.success(), "{refused:?}");

    // Positive control: the shared host /tmp stays writable.
    let written = cwd.path().join("written");
    let output = launch(
        &fixture,
        cwd.path(),
        &["touch", written.to_str().unwrap()],
        b"",
    );
    assert!(output.status.success(), "{output:?}");
    assert!(written.exists());
}

#[test]
fn missing_and_non_executable_targets_use_shell_statuses() {
    let Some(fixture) = ready_fixture() else {
        return;
    };
    let cwd = tmp_dir();
    let missing = launch(&fixture, cwd.path(), &["no-such-command"], b"");
    assert_eq!(missing.status.code(), Some(127), "{missing:?}");
    let denied = launch(&fixture, cwd.path(), &["/bin/not-executable"], b"");
    assert_eq!(denied.status.code(), Some(126), "{denied:?}");
    // Positive control: the same tree runs an executable target.
    let ran = launch(&fixture, cwd.path(), &["/bin/sh", "-c", "exit 0"], b"");
    assert_eq!(ran.status.code(), Some(0), "{ran:?}");
}

#[test]
fn refusals_before_namespaces_exit_with_the_launcher_status() {
    // These refusals happen before unshare, so they need no user namespace.
    let Some(fixture) = build_tree() else { return };
    let cwd = tmp_dir();
    let missing_tree = Command::new(LAUNCHER)
        .args(["--tree", "/nonexistent/conary-exec-tree", "--", "sh"])
        .current_dir(cwd.path())
        .output()
        .unwrap();
    assert_eq!(missing_tree.status.code(), Some(125), "{missing_tree:?}");

    let outside = Command::new(LAUNCHER)
        .arg("--tree")
        .arg(&fixture.tree)
        .args(["--", "sh", "-c", "exit 0"])
        .current_dir("/")
        .output()
        .unwrap();
    assert_eq!(outside.status.code(), Some(125), "{outside:?}");
}

#[test]
fn elf_interpreter_detection_reads_program_headers() {
    assert_eq!(elf_has_interpreter(b"not an elf"), None);
    let mut header = vec![0_u8; 64 + 56];
    header[..6].copy_from_slice(b"\x7fELF\x02\x01");
    header[32..40].copy_from_slice(&64_u64.to_le_bytes());
    header[54..56].copy_from_slice(&56_u16.to_le_bytes());
    header[56..58].copy_from_slice(&1_u16.to_le_bytes());
    assert_eq!(elf_has_interpreter(&header), Some(false));
    header[64..68].copy_from_slice(&3_u32.to_le_bytes());
    assert_eq!(elf_has_interpreter(&header), Some(true));
}
