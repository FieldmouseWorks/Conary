// apps/conary-test/src/container/image/tests.rs

#![cfg(test)]

use super::static_shell::{
    STATIC_TEST_SHELL_ENV, StaticShellError, probe_static_shell, resolve_static_test_shell,
    validate_static_shell,
};
use super::{
    NativePackageArtifact, ShellProviderRequirement, find_project_root, resolve_stage_source,
    stage_build_context, stage_native_package,
};
use crate::config::DistroBuildContext;
use crate::config::TestManifest;
use crate::config::manifest::StaticFixture;
use conary_core::repository::supported_profiles::ProfilePackageFormat;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::ffi::OsString;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::sync::MutexGuard;

/// Serialize process-environment mutation with the rest of the crate and
/// restore every touched variable when the guard drops.
#[cfg(unix)]
struct EnvGuard {
    _lock: MutexGuard<'static, ()>,
    values: Vec<(&'static str, Option<OsString>)>,
}

#[cfg(unix)]
impl EnvGuard {
    fn new(names: &[&'static str]) -> Self {
        let lock = crate::test_support::lock_env();
        let values = names
            .iter()
            .map(|name| (*name, std::env::var_os(name)))
            .collect();
        Self {
            _lock: lock,
            values,
        }
    }

    fn set(&self, name: &str, value: &std::ffi::OsStr) {
        // SAFETY: environment mutation is serialized by the crate-wide test lock.
        unsafe {
            std::env::set_var(name, value);
        }
    }

    fn clear(&self, name: &str) {
        // SAFETY: environment mutation is serialized by the crate-wide test lock.
        unsafe {
            std::env::remove_var(name);
        }
    }
}

#[cfg(unix)]
impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (name, value) in &self.values {
            // SAFETY: the lock is still held while the original values are restored.
            unsafe {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
}

const SYNTHETIC_ELF_HEADER_LEN: usize = 64;
const SYNTHETIC_PROGRAM_HEADER_LEN: usize = 56;
const SYNTHETIC_E_TYPE_OFFSET: usize = 0x10;
const SYNTHETIC_E_MACHINE_OFFSET: usize = 0x12;
const SYNTHETIC_E_ENTRY_OFFSET: usize = 0x18;
const SYNTHETIC_E_PHOFF_OFFSET: usize = 0x20;
const SYNTHETIC_E_PHENTSIZE_OFFSET: usize = 0x36;
const SYNTHETIC_E_PHNUM_OFFSET: usize = 0x38;
const SYNTHETIC_P_FLAGS_OFFSET: usize = 4;
const SYNTHETIC_P_VADDR_OFFSET: usize = 16;
const SYNTHETIC_P_MEMSZ_OFFSET: usize = 40;

const SYNTHETIC_ET_EXEC: u16 = 2;
const SYNTHETIC_ET_DYN: u16 = 3;
const SYNTHETIC_EM_X86_64: u16 = 0x3e;
const SYNTHETIC_EM_AARCH64: u16 = 0xb7;

const SYNTHETIC_PT_LOAD: u32 = 1;
const SYNTHETIC_PT_DYNAMIC: u32 = 2;
const SYNTHETIC_PT_INTERP: u32 = 3;

const SYNTHETIC_PF_X: u32 = 0x1;
const SYNTHETIC_PF_R: u32 = 0x4;

const SYNTHETIC_DT_NULL: i64 = 0;
const SYNTHETIC_DT_NEEDED: i64 = 1;

/// A program header the synthetic ELF fixture emits.
enum SyntheticSegment<'a> {
    /// A loadable segment with the virtual address, permission flags, and file
    /// and memory sizes the validator inspects.
    Load {
        vaddr: u64,
        flags: u32,
        filesz: u64,
        memsz: u64,
    },
    Interp,
    Dynamic(&'a [i64]),
}

/// A read-execute load segment with file content: the runnable shape the
/// validator accepts.
fn executable_load(vaddr: u64, filesz: u64, memsz: u64) -> SyntheticSegment<'static> {
    SyntheticSegment::Load {
        vaddr,
        flags: SYNTHETIC_PF_R | SYNTHETIC_PF_X,
        filesz,
        memsz,
    }
}

/// Build a minimal ELF64 image carrying exactly the requested program headers.
///
/// Real busybox is not required: the validator only inspects the ELF header and
/// program headers, so a synthetic image proves each accepted and refused case
/// deterministically on any host. The load-segment fields are explicit so a
/// malformed shape (no execute bit, no file content, unreachable entry point)
/// can be exercised without a real binary.
fn synthetic_elf(
    e_type: u16,
    e_machine: u16,
    e_entry: u64,
    segments: &[SyntheticSegment<'_>],
) -> Vec<u8> {
    let program_header_end =
        SYNTHETIC_ELF_HEADER_LEN + SYNTHETIC_PROGRAM_HEADER_LEN * segments.len();

    let mut payload = Vec::new();
    // p_type, p_offset, p_filesz, p_flags, p_vaddr, p_memsz
    let mut headers = Vec::with_capacity(segments.len());
    for segment in segments {
        match segment {
            SyntheticSegment::Load {
                vaddr,
                flags,
                filesz,
                memsz,
            } => headers.push((SYNTHETIC_PT_LOAD, 0, *filesz, *flags, *vaddr, *memsz)),
            SyntheticSegment::Interp => {
                let offset = program_header_end + payload.len();
                let interpreter = b"/lib64/ld-linux-x86-64.so.2";
                payload.extend_from_slice(interpreter);
                payload.push(0);
                headers.push((
                    SYNTHETIC_PT_INTERP,
                    offset as u64,
                    interpreter.len() as u64,
                    0,
                    0,
                    0,
                ));
            }
            SyntheticSegment::Dynamic(tags) => {
                let offset = program_header_end + payload.len();
                for tag in *tags {
                    payload.extend_from_slice(&tag.to_le_bytes());
                    payload.extend_from_slice(&0i64.to_le_bytes());
                }
                headers.push((
                    SYNTHETIC_PT_DYNAMIC,
                    offset as u64,
                    (tags.len() * 16) as u64,
                    0,
                    0,
                    0,
                ));
            }
        }
    }

    let mut image = vec![0u8; program_header_end + payload.len()];
    image[..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
    image[4] = 2;
    image[5] = 1;
    image[SYNTHETIC_E_TYPE_OFFSET..SYNTHETIC_E_TYPE_OFFSET + 2]
        .copy_from_slice(&e_type.to_le_bytes());
    image[SYNTHETIC_E_MACHINE_OFFSET..SYNTHETIC_E_MACHINE_OFFSET + 2]
        .copy_from_slice(&e_machine.to_le_bytes());
    image[SYNTHETIC_E_ENTRY_OFFSET..SYNTHETIC_E_ENTRY_OFFSET + 8]
        .copy_from_slice(&e_entry.to_le_bytes());
    image[SYNTHETIC_E_PHOFF_OFFSET..SYNTHETIC_E_PHOFF_OFFSET + 8]
        .copy_from_slice(&(SYNTHETIC_ELF_HEADER_LEN as u64).to_le_bytes());
    image[SYNTHETIC_E_PHENTSIZE_OFFSET..SYNTHETIC_E_PHENTSIZE_OFFSET + 2]
        .copy_from_slice(&(SYNTHETIC_PROGRAM_HEADER_LEN as u16).to_le_bytes());
    image[SYNTHETIC_E_PHNUM_OFFSET..SYNTHETIC_E_PHNUM_OFFSET + 2]
        .copy_from_slice(&(segments.len() as u16).to_le_bytes());

    for (index, (p_type, p_offset, p_filesz, p_flags, p_vaddr, p_memsz)) in
        headers.iter().enumerate()
    {
        let base = SYNTHETIC_ELF_HEADER_LEN + index * SYNTHETIC_PROGRAM_HEADER_LEN;
        image[base..base + 4].copy_from_slice(&p_type.to_le_bytes());
        image[base + SYNTHETIC_P_FLAGS_OFFSET..base + SYNTHETIC_P_FLAGS_OFFSET + 4]
            .copy_from_slice(&p_flags.to_le_bytes());
        image[base + 8..base + 16].copy_from_slice(&p_offset.to_le_bytes());
        image[base + SYNTHETIC_P_VADDR_OFFSET..base + SYNTHETIC_P_VADDR_OFFSET + 8]
            .copy_from_slice(&p_vaddr.to_le_bytes());
        image[base + 32..base + 40].copy_from_slice(&p_filesz.to_le_bytes());
        image[base + SYNTHETIC_P_MEMSZ_OFFSET..base + SYNTHETIC_P_MEMSZ_OFFSET + 8]
            .copy_from_slice(&p_memsz.to_le_bytes());
    }
    image[program_header_end..].copy_from_slice(&payload);

    image
}

/// A statically linked `ET_EXEC` x86_64 image: the shape a usable provider has.
fn static_shell_elf() -> Vec<u8> {
    synthetic_elf(
        SYNTHETIC_ET_EXEC,
        SYNTHETIC_EM_X86_64,
        0x40_1000,
        &[executable_load(0x40_0000, 0x1000, 0x2000)],
    )
}

#[cfg(unix)]
fn write_executable(path: &Path, contents: &[u8]) {
    fs::write(path, contents).expect("write executable fixture");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("make fixture executable");
}

#[test]
#[cfg(unix)]
fn validate_static_shell_accepts_static_exec_elf() {
    let directory = tempfile::tempdir().expect("create temp directory");
    let shell = directory.path().join("static-sh");
    write_executable(&shell, &static_shell_elf());

    validate_static_shell(&shell).expect("a static ET_EXEC shell must be accepted");
}

/// Find a real `busybox` on `PATH` that passes both the structural check and the
/// functional probe.
///
/// Callers must hold the crate environment lock so the `PATH` read does not race
/// another test's mutation.
#[cfg(unix)]
fn usable_busybox_on_path() -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|directory| directory.join("busybox"))
        .find(|candidate| {
            validate_static_shell(candidate).is_ok() && probe_static_shell(candidate).is_ok()
        })
}

#[test]
#[cfg(unix)]
fn resolve_static_test_shell_uses_a_probe_passing_explicit_shell() {
    let env = EnvGuard::new(&[STATIC_TEST_SHELL_ENV]);
    let Some(shell) = usable_busybox_on_path() else {
        return;
    };

    env.set(STATIC_TEST_SHELL_ENV, shell.as_os_str());

    assert_eq!(
        resolve_static_test_shell().expect("a probe-passing static shell must be selected"),
        shell
    );
}

#[test]
#[cfg(unix)]
fn resolve_static_test_shell_rejects_missing_explicit_path() {
    let env = EnvGuard::new(&[STATIC_TEST_SHELL_ENV]);
    let directory = tempfile::tempdir().expect("create temp directory");
    let missing = directory.path().join("absent-shell");
    env.set(STATIC_TEST_SHELL_ENV, missing.as_os_str());

    let error = resolve_static_test_shell().expect_err("missing explicit path must be rejected");
    assert!(
        matches!(
            error.downcast_ref::<StaticShellError>(),
            Some(StaticShellError::ExplicitNotExecutable { .. })
        ),
        "unexpected error: {error}"
    );
}

#[test]
#[cfg(unix)]
fn resolve_static_test_shell_finds_static_busybox_first_on_path() {
    let env = EnvGuard::new(&[STATIC_TEST_SHELL_ENV, "PATH"]);
    env.clear(STATIC_TEST_SHELL_ENV);
    let Some(busybox) = usable_busybox_on_path() else {
        return;
    };

    let directory = tempfile::tempdir().expect("create temp directory");
    let candidate = directory.path().join("busybox");
    std::os::unix::fs::symlink(&busybox, &candidate).expect("link the busybox candidate");

    let original = std::env::var_os("PATH").unwrap_or_default();
    let mut entries = vec![directory.path().to_path_buf()];
    entries.extend(std::env::split_paths(&original));
    let controlled = std::env::join_paths(entries).expect("join controlled PATH");
    env.set("PATH", controlled.as_os_str());

    assert_eq!(
        resolve_static_test_shell().expect("static busybox on PATH must be selected"),
        candidate
    );
}

#[test]
#[cfg(unix)]
fn validate_static_shell_rejects_program_interpreter() {
    let directory = tempfile::tempdir().expect("create temp directory");
    let shell = directory.path().join("dynamic-sh");
    write_executable(
        &shell,
        &synthetic_elf(
            SYNTHETIC_ET_EXEC,
            SYNTHETIC_EM_X86_64,
            0x40_1000,
            &[
                executable_load(0x40_0000, 0x1000, 0x2000),
                SyntheticSegment::Interp,
            ],
        ),
    );

    let error = validate_static_shell(&shell).expect_err("dynamic shell must be rejected");
    assert!(
        matches!(error, StaticShellError::RequestsInterpreter { .. }),
        "unexpected error: {error}"
    );
}

#[test]
#[cfg(unix)]
fn validate_static_shell_rejects_script() {
    let directory = tempfile::tempdir().expect("create temp directory");
    let shell = directory.path().join("script-sh");
    write_executable(&shell, b"#!/bin/sh\nexit 0\n");

    let error = validate_static_shell(&shell).expect_err("a script shell must be rejected");
    assert!(
        matches!(error, StaticShellError::NotElf64 { .. }),
        "unexpected error: {error}"
    );
}

#[test]
#[cfg(unix)]
fn validate_static_shell_rejects_wrong_machine() {
    let directory = tempfile::tempdir().expect("create temp directory");
    let shell = directory.path().join("aarch64-sh");
    write_executable(
        &shell,
        &synthetic_elf(
            SYNTHETIC_ET_EXEC,
            SYNTHETIC_EM_AARCH64,
            0x40_1000,
            &[executable_load(0x40_0000, 0x1000, 0x2000)],
        ),
    );

    let error = validate_static_shell(&shell).expect_err("wrong machine must be rejected");
    assert!(
        matches!(
            error,
            StaticShellError::WrongMachine { machine, .. } if machine == SYNTHETIC_EM_AARCH64
        ),
        "unexpected error: {error}"
    );
}

#[test]
#[cfg(unix)]
fn validate_static_shell_rejects_dynamic_dependencies() {
    let directory = tempfile::tempdir().expect("create temp directory");
    let shell = directory.path().join("needed-sh");
    write_executable(
        &shell,
        &synthetic_elf(
            SYNTHETIC_ET_DYN,
            SYNTHETIC_EM_X86_64,
            0x100,
            &[
                executable_load(0, 0x1000, 0x1000),
                SyntheticSegment::Dynamic(&[SYNTHETIC_DT_NEEDED, SYNTHETIC_DT_NULL]),
            ],
        ),
    );

    let error = validate_static_shell(&shell).expect_err("DT_NEEDED shell must be rejected");
    assert!(
        matches!(error, StaticShellError::HasDynamicDependencies { .. }),
        "unexpected error: {error}"
    );
}

#[test]
#[cfg(unix)]
fn validate_static_shell_accepts_static_pie_without_dependencies() {
    let directory = tempfile::tempdir().expect("create temp directory");
    let shell = directory.path().join("static-pie-sh");
    write_executable(
        &shell,
        &synthetic_elf(
            SYNTHETIC_ET_DYN,
            SYNTHETIC_EM_X86_64,
            0x100,
            &[
                executable_load(0, 0x1000, 0x1000),
                SyntheticSegment::Dynamic(&[SYNTHETIC_DT_NULL]),
            ],
        ),
    );

    validate_static_shell(&shell).expect("static-PIE shell must be accepted");
}

#[test]
#[cfg(unix)]
fn validate_static_shell_rejects_load_without_execute() {
    let directory = tempfile::tempdir().expect("create temp directory");
    let shell = directory.path().join("read-only-sh");
    write_executable(
        &shell,
        &synthetic_elf(
            SYNTHETIC_ET_EXEC,
            SYNTHETIC_EM_X86_64,
            0x40_1000,
            &[SyntheticSegment::Load {
                vaddr: 0x40_0000,
                flags: SYNTHETIC_PF_R,
                filesz: 0x1000,
                memsz: 0x2000,
            }],
        ),
    );

    let error = validate_static_shell(&shell).expect_err("a load without execute must be refused");
    assert!(
        matches!(error, StaticShellError::NoExecutableLoadSegment { .. }),
        "unexpected error: {error}"
    );
}

#[test]
#[cfg(unix)]
fn validate_static_shell_rejects_empty_executable_load() {
    let directory = tempfile::tempdir().expect("create temp directory");
    let shell = directory.path().join("empty-load-sh");
    write_executable(
        &shell,
        &synthetic_elf(
            SYNTHETIC_ET_EXEC,
            SYNTHETIC_EM_X86_64,
            0x40_1000,
            &[SyntheticSegment::Load {
                vaddr: 0x40_0000,
                flags: SYNTHETIC_PF_R | SYNTHETIC_PF_X,
                filesz: 0,
                memsz: 0x2000,
            }],
        ),
    );

    let error =
        validate_static_shell(&shell).expect_err("an executable load with no content is refused");
    assert!(
        matches!(error, StaticShellError::NoExecutableLoadSegment { .. }),
        "unexpected error: {error}"
    );
}

#[test]
#[cfg(unix)]
fn validate_static_shell_rejects_entry_outside_executable_load() {
    let directory = tempfile::tempdir().expect("create temp directory");
    let shell = directory.path().join("unreachable-entry-sh");
    write_executable(
        &shell,
        &synthetic_elf(
            SYNTHETIC_ET_EXEC,
            SYNTHETIC_EM_X86_64,
            0x50_0000,
            &[executable_load(0x40_0000, 0x1000, 0x2000)],
        ),
    );

    let error =
        validate_static_shell(&shell).expect_err("an entry outside the load must be refused");
    assert!(
        matches!(
            error,
            StaticShellError::EntryPointOutsideExecutableLoad { entry, .. } if entry == 0x50_0000
        ),
        "unexpected error: {error}"
    );
}

#[test]
#[cfg(unix)]
fn probe_static_shell_accepts_real_busybox() {
    let _env = EnvGuard::new(&["PATH"]);
    let Some(busybox) = usable_busybox_on_path() else {
        return;
    };

    probe_static_shell(&busybox).expect("a standalone-applet busybox must pass the probe");
}

#[test]
#[cfg(unix)]
fn probe_static_shell_rejects_a_shell_that_exits_nonzero() {
    let directory = tempfile::tempdir().expect("create temp directory");
    let shell = directory.path().join("failing-sh");
    write_executable(&shell, b"#!/bin/sh\nexit 1\n");

    let error = probe_static_shell(&shell).expect_err("a non-zero shell must be refused");
    assert!(
        matches!(error, StaticShellError::MissingHookUtilities { .. }),
        "unexpected error: {error}"
    );
}

#[test]
fn shell_provider_requirement_comes_only_from_the_typed_declaration() {
    let declaring = manifest_with_suite(&["conary-test-shell"], "true");
    assert_eq!(
        ShellProviderRequirement::from_manifests([&declaring]),
        ShellProviderRequirement::Shell
    );

    let base = manifest_with_suite(&["conary-test-base"], "true");
    assert_eq!(
        ShellProviderRequirement::from_manifests([&base]),
        ShellProviderRequirement::Base
    );

    let both = manifest_with_suite(&["conary-test-shell", "conary-test-base"], "true");
    assert_eq!(
        ShellProviderRequirement::from_manifests([&both]),
        ShellProviderRequirement::ShellAndBase
    );
    assert!(ShellProviderRequirement::ShellAndBase.shell_required());
    assert!(ShellProviderRequirement::ShellAndBase.base_required());
    assert!(!ShellProviderRequirement::Base.shell_required());
    assert!(!ShellProviderRequirement::Shell.base_required());

    // Command text that mentions (or quotes) a fixture variable must not opt
    // the image in; only the typed declaration does.
    let quoted_shell = manifest_with_suite(
        &[],
        r#"ccs install "${FIXTURE_SHELL_CCS}" --policy ${FIXTURE_CCS_POLICY} --sandbox always --yes"#,
    );
    assert_eq!(
        ShellProviderRequirement::from_manifests([&quoted_shell]),
        ShellProviderRequirement::NotInstalled
    );

    let quoted_base = manifest_with_suite(
        &[],
        r#"ccs install "${FIXTURE_BASE_CCS}" --policy ${FIXTURE_CCS_POLICY} --sandbox always --yes"#,
    );
    assert_eq!(
        ShellProviderRequirement::from_manifests([&quoted_base]),
        ShellProviderRequirement::NotInstalled
    );
}

fn manifest_with_suite(fixtures: &[&str], command: &str) -> TestManifest {
    let fixtures = fixtures
        .iter()
        .map(|fixture| format!("\"{fixture}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let document = format!(
        r#"
[suite]
name = "shell-requirement"
phase = 2
requires_fixtures = [{fixtures}]

[[suite.setup]]
conary = '{command}'

[[test]]
id = "T01"
name = "demo"
description = "demo"
timeout = 10

[[test.step]]
run = "true"
"#
    );
    toml::from_str(&document).expect("parse synthetic manifest")
}

#[test]
#[cfg(unix)]
fn filtered_copy_preserves_directory_modes() {
    let source = tempfile::tempdir().expect("create source directory");
    let destination = tempfile::tempdir().expect("create destination directory");
    let fixture = source.path().join("fixture");
    let nested = fixture.join("usr/bin");
    fs::create_dir_all(&nested).expect("create fixture tree");
    fs::set_permissions(&fixture, fs::Permissions::from_mode(0o751))
        .expect("set fixture root mode");
    fs::set_permissions(fixture.join("usr"), fs::Permissions::from_mode(0o755))
        .expect("set usr mode");
    fs::set_permissions(&nested, fs::Permissions::from_mode(0o750)).expect("set nested mode");

    let copied = destination.path().join("fixture");
    super::copy_dir_filtered(&fixture, &copied, &[]).expect("copy fixture tree");

    assert_eq!(
        fs::metadata(&copied)
            .expect("copied root metadata")
            .permissions()
            .mode()
            & 0o7777,
        0o751
    );
    assert_eq!(
        fs::metadata(copied.join("usr"))
            .expect("copied usr metadata")
            .permissions()
            .mode()
            & 0o7777,
        0o755
    );
    assert_eq!(
        fs::metadata(copied.join("usr/bin"))
            .expect("copied nested metadata")
            .permissions()
            .mode()
            & 0o7777,
        0o750
    );
}

#[test]
fn stage_build_context_creates_small_remi_context() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_nanos();
    let project_root = std::env::temp_dir().join(format!("conary-test-stage-context-{unique}"));
    let remi_root = project_root.join("apps/conary/tests/integration/remi");
    let containerfile = remi_root.join("containers/Containerfile.fedora44");

    fs::create_dir_all(remi_root.join("containers")).expect("create containers");
    fs::create_dir_all(project_root.join("apps/conary/tests/fixtures/recipes/simple-hello"))
        .expect("create fixtures");
    fs::create_dir_all(
        project_root.join("apps/conary/tests/fixtures/conary-test-fixture/v1/output"),
    )
    .expect("create fixture output");
    fs::create_dir_all(project_root.join("apps/conary/tests/fixtures/ccs-test-authority"))
        .expect("create fixture authority");
    fs::create_dir_all(project_root.join("packaging/arch")).expect("create packaging");

    fs::write(
        project_root.join("Cargo.toml"),
        "[workspace]\nmembers = []\n",
    )
    .expect("write cargo");
    fs::write(project_root.join("conary"), "binary").expect("write binary");
    fs::write(&containerfile, "FROM scratch\n").expect("write containerfile");
    fs::write(remi_root.join("config.toml"), "[paths]\n").expect("write config");
    fs::write(
        project_root.join("apps/conary/tests/fixtures/recipes/simple-hello/recipe.toml"),
        "name = 'simple-hello'\n",
    )
    .expect("write fixture");
    fs::write(
        project_root.join("apps/conary/tests/fixtures/conary-test-fixture/v1/output/test.ccs"),
        "fixture-bytes",
    )
    .expect("write fixture output");
    fs::write(
        project_root
            .join("apps/conary/tests/fixtures/ccs-test-authority/fixture-signing-key.private"),
        "test private key\n",
    )
    .expect("write fixture private key");
    fs::write(
        project_root.join("apps/conary/tests/fixtures/ccs-test-authority/trust-policy.toml"),
        "trusted_keys = [\"test\"]\n",
    )
    .expect("write fixture trust policy");
    fs::write(
        project_root.join("packaging/arch/PKGBUILD"),
        "pkgname=conary\n",
    )
    .expect("write pkgbuild");

    let staged = stage_build_context(
        &containerfile,
        "fedora44",
        DistroBuildContext::Binary,
        None,
        ShellProviderRequirement::NotInstalled,
    )
    .expect("stage build context");

    assert!(
        staged
            .root
            .join("containers/Containerfile.fedora44")
            .is_file()
    );
    assert!(staged.root.join("config.toml").is_file());
    assert!(
        staged
            .root
            .join("fixtures/recipes/simple-hello/recipe.toml")
            .is_file()
    );
    assert!(
        staged
            .root
            .join("fixtures/conary-test-fixture/v1/output/test.ccs")
            .is_file()
    );
    assert!(staged.root.join("fixtures/pkgbuild/PKGBUILD").is_file());
    assert!(staged.root.join("conary").is_file());
    assert!(!staged.root.join("target").exists());
    assert!(!staged.root.join("source").exists());

    drop(staged);

    let package = project_root.join("release.rpm");
    fs::write(&package, "published package bytes").expect("write release package");
    let staged = stage_build_context(
        &containerfile,
        "fedora44",
        DistroBuildContext::Binary,
        Some(NativePackageArtifact {
            path: &package,
            format: ProfilePackageFormat::Rpm,
        }),
        ShellProviderRequirement::NotInstalled,
    )
    .expect("stage native package build context");
    assert_eq!(
        fs::read(staged.root.join("conary-release.rpm")).expect("read staged package"),
        b"published package bytes"
    );

    drop(staged);
    fs::remove_dir_all(project_root).expect("cleanup project root");
}

/// Fake `conary` used by the phase-2 staging fixtures. It emits exactly one
/// `.ccs` per fixture manifest and accepts every `ccs verify`.
#[cfg(unix)]
const PHASE2_FAKE_CONARY: &str = r#"#!/usr/bin/env bash
set -euo pipefail
if [[ "$1 $2" == "ccs verify" ]]; then
  [[ "$4" == "--policy" ]]
  exit 0
fi
manifest="$3"
output=""
for ((i = 1; i <= $#; i++)); do
  if [[ "${!i}" == "--output" ]]; then
    next=$((i + 1))
    output="${!next}"
  fi
done
case "$manifest" in
  */v1/ccs.toml) file="conary-test-fixture-1.0.0-1.ccs" ;;
  */v2/ccs.toml) file="conary-test-fixture-2.0.0-1.ccs" ;;
  */conary-test-shell/ccs.toml) file="conary-test-shell-1.0.0-1.ccs" ;;
  */conary-test-base/ccs.toml) file="conary-test-base-1.0.0-1.ccs" ;;
  *) echo "unexpected manifest: $manifest" >&2; exit 2 ;;
esac
[[ -n "$output" ]]
mkdir -p "$output"
printf 'fixture\n' > "$output/$file"
"#;

/// Lay out the minimal fixture workspace the phase-2 staging tests need and
/// return the containerfile path. Includes a fake host `conary`.
#[cfg(unix)]
fn phase2_fixture_project(project_root: &Path) -> PathBuf {
    let remi_root = project_root.join("apps/conary/tests/integration/remi");
    let fixture_root = project_root.join("apps/conary/tests/fixtures/conary-test-fixture");
    let shell_fixture_root = project_root.join("apps/conary/tests/fixtures/conary-test-shell");
    let base_fixture_root = project_root.join("apps/conary/tests/fixtures/conary-test-base");
    let authority_root = project_root.join("apps/conary/tests/fixtures/ccs-test-authority");
    let containerfile = remi_root.join("containers/Containerfile.arch");

    fs::create_dir_all(remi_root.join("containers")).expect("create containers");
    fs::create_dir_all(fixture_root.join("v1/stage/usr/share/conary-test"))
        .expect("create v1 fixture source");
    fs::create_dir_all(fixture_root.join("v2/stage/usr/share/conary-test"))
        .expect("create v2 fixture source");
    fs::create_dir_all(&shell_fixture_root).expect("create shell fixture directory");
    fs::create_dir_all(&base_fixture_root).expect("create base fixture directory");
    fs::create_dir_all(&authority_root).expect("create fixture authority");
    fs::write(
        project_root.join("Cargo.toml"),
        "[workspace]\nmembers = []\n",
    )
    .expect("write cargo");
    fs::write(&containerfile, "FROM scratch\n").expect("write containerfile");
    fs::write(remi_root.join("config.toml"), "[paths]\n").expect("write config");
    fs::write(fixture_root.join("v1/ccs.toml"), "[package]\n").expect("write v1 ccs");
    fs::write(fixture_root.join("v2/ccs.toml"), "[package]\n").expect("write v2 ccs");
    fs::write(
        shell_fixture_root.join("ccs.toml"),
        "[package]\nname = \"conary-test-shell\"\n",
    )
    .expect("write shell ccs");
    fs::write(
        base_fixture_root.join("ccs.toml"),
        "[package]\nname = \"conary-test-base\"\n",
    )
    .expect("write base ccs");
    fs::write(
        fixture_root.join("v1/stage/usr/share/conary-test/hello.txt"),
        "hello v1\n",
    )
    .expect("write v1 source");
    fs::write(
        fixture_root.join("v2/stage/usr/share/conary-test/hello.txt"),
        "hello v2\n",
    )
    .expect("write v2 source");
    fs::write(
        authority_root.join("fixture-signing-key.private"),
        "test private key\n",
    )
    .expect("write fixture private key");
    fs::write(
        authority_root.join("trust-policy.toml"),
        "trusted_keys = [\"test\"]\n",
    )
    .expect("write fixture trust policy");
    write_executable(&project_root.join("conary"), PHASE2_FAKE_CONARY.as_bytes());

    containerfile
}

#[test]
#[cfg(unix)]
fn stage_build_context_generates_missing_phase2_fixture_outputs() {
    let env = EnvGuard::new(&[STATIC_TEST_SHELL_ENV]);
    let Some(static_shell) = usable_busybox_on_path() else {
        return;
    };
    env.set(STATIC_TEST_SHELL_ENV, static_shell.as_os_str());

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_nanos();
    let project_root = std::env::temp_dir().join(format!("conary-test-phase2-fixtures-{unique}"));
    let containerfile = phase2_fixture_project(&project_root);

    let staged = stage_build_context(
        &containerfile,
        "arch",
        DistroBuildContext::Binary,
        None,
        ShellProviderRequirement::ShellAndBase,
    )
    .expect("stage build context");

    assert!(
        staged
            .root
            .join("fixtures/conary-test-fixture/v1/output/conary-test-fixture-1.0.0-1.ccs")
            .is_file()
    );
    assert!(
        staged
            .root
            .join("fixtures/conary-test-fixture/v2/output/conary-test-fixture-2.0.0-1.ccs")
            .is_file()
    );
    let shell_artifact =
        super::static_fixture_artifact_path(StaticFixture::Shell, &staged.root.join("fixtures"));
    assert!(
        shell_artifact.is_file(),
        "the shell provider fixture must be built and staged at {}",
        shell_artifact.display()
    );
    assert!(
        staged
            .root
            .join("fixtures/conary-test-base/output/conary-test-base-1.0.0-1.ccs")
            .is_file(),
        "the base provider fixture must be built and staged for the image"
    );

    // The base stages the exact boot layout the generation builder resolves for
    // a staged boot root. The bytes are clearly fake and are never booted.
    let base_stage = staged.root.join("fixtures/conary-test-base/stage");
    assert!(base_stage.join("sbin/init").is_file());
    assert!(base_stage.join("boot/vmlinuz-conary-test").is_file());
    assert!(base_stage.join("boot/initramfs-conary-test.img").is_file());
    assert!(base_stage.join("boot/EFI/BOOT/BOOTX64.EFI").is_file());

    drop(staged);
    fs::remove_dir_all(project_root).expect("cleanup project root");
}

/// Positive control: the same fixture workspace with `ShellAndBase` stages the
/// provider artifacts; without a requirement it must not consult a host binary
/// at all, even when `CONARY_TEST_STATIC_SHELL` names an unusable path.
#[test]
#[cfg(unix)]
fn stage_build_context_skips_provider_fixtures_without_requirement() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_nanos();
    let project_root = std::env::temp_dir().join(format!("conary-test-no-shell-{unique}"));
    let containerfile = phase2_fixture_project(&project_root);

    let absent_shell = project_root.join("absent-shell");
    let env = EnvGuard::new(&[STATIC_TEST_SHELL_ENV]);
    env.set(STATIC_TEST_SHELL_ENV, absent_shell.as_os_str());

    let staged = stage_build_context(
        &containerfile,
        "arch",
        DistroBuildContext::Binary,
        None,
        ShellProviderRequirement::NotInstalled,
    )
    .expect("image staging must not require a host binary");

    assert!(
        staged
            .root
            .join("fixtures/conary-test-fixture/v1/output/conary-test-fixture-1.0.0-1.ccs")
            .is_file(),
        "the primary fixture still builds without a host binary"
    );
    let shell_output =
        super::static_fixture_artifact_path(StaticFixture::Shell, &staged.root.join("fixtures"))
            .parent()
            .expect("the shell artifact path has an output directory")
            .to_path_buf();
    assert!(
        !shell_output.exists(),
        "no selected suite installs the shell provider, so it must not be staged"
    );
    assert!(
        !staged
            .root
            .join("fixtures/conary-test-base/output")
            .exists(),
        "no selected suite installs the base provider, so it must not be staged"
    );

    drop(staged);
    fs::remove_dir_all(project_root).expect("cleanup project root");
}

/// The base provider is staged on its own when only it is required, and the
/// shell provider it shares no fixture directory with is left alone.
#[test]
#[cfg(unix)]
fn stage_build_context_generates_only_the_required_provider() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_nanos();
    let project_root = std::env::temp_dir().join(format!("conary-test-base-only-{unique}"));
    let containerfile = phase2_fixture_project(&project_root);

    let static_binary = project_root.join("static-base");
    write_executable(&static_binary, &static_shell_elf());
    let env = EnvGuard::new(&[STATIC_TEST_SHELL_ENV]);
    env.set(STATIC_TEST_SHELL_ENV, static_binary.as_os_str());

    let staged = stage_build_context(
        &containerfile,
        "arch",
        DistroBuildContext::Binary,
        None,
        ShellProviderRequirement::Base,
    )
    .expect("a base-only image must accept an ELF-validated static binary without a shell probe");

    assert!(
        staged
            .root
            .join("fixtures/conary-test-base/output/conary-test-base-1.0.0-1.ccs")
            .is_file(),
        "the base provider fixture must be built when a suite installs it"
    );
    assert!(
        !staged
            .root
            .join("fixtures/conary-test-shell/output")
            .exists(),
        "the shell provider is not installed by the selected suite, so it must not be staged"
    );

    drop(staged);
    fs::remove_dir_all(project_root).expect("cleanup project root");
}

#[test]
fn find_project_root_prefers_workspace_root_over_nested_package() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_nanos();
    let workspace_root = std::env::temp_dir().join(format!("conary-test-workspace-root-{unique}"));
    let integration_root = workspace_root.join("apps/conary/tests/integration/remi");

    fs::create_dir_all(integration_root.join("containers")).expect("create integration tree");
    fs::write(
        workspace_root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"apps/conary\"]\n",
    )
    .expect("write workspace cargo");
    fs::create_dir_all(workspace_root.join("apps/conary")).expect("create nested app");
    fs::write(
        workspace_root.join("apps/conary/Cargo.toml"),
        "[package]\nname = \"conary\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("write nested package cargo");

    let found = find_project_root(Path::new(&integration_root)).expect("find project root");
    assert_eq!(found, workspace_root);

    fs::remove_dir_all(workspace_root).expect("cleanup workspace root");
}

/// Which binary reaches the image is a typed decision, and the static
/// choice fails closed rather than quietly staging the host build.
#[test]
fn staged_binary_is_selected_by_typed_capability() {
    let target_dir = tempfile::tempdir().expect("create temp target directory");
    let host_binary = Path::new("/workspace/target/debug/conary");

    assert_eq!(
        resolve_stage_source(DistroBuildContext::Binary, host_binary, target_dir.path())
            .expect("host staging needs no static artifact"),
        host_binary,
        "the binary capability must stage the host build"
    );

    let error = resolve_stage_source(
        DistroBuildContext::StaticBinary,
        host_binary,
        target_dir.path(),
    )
    .expect_err("a missing static artifact must not fall back to the host binary");
    assert!(
        error
            .to_string()
            .contains("static conary artifact not found"),
        "unexpected message: {error}"
    );

    let artifact = crate::static_binary::static_conary_binary_path_in(target_dir.path());
    fs::create_dir_all(artifact.parent().expect("artifact parent"))
        .expect("create musl profile directory");
    fs::write(
        &artifact,
        crate::static_binary::synthetic_elf(&[crate::static_binary::PT_LOAD]),
    )
    .expect("write static artifact");

    assert_eq!(
        resolve_stage_source(
            DistroBuildContext::StaticBinary,
            host_binary,
            target_dir.path()
        )
        .expect("static staging must accept a static artifact"),
        artifact,
        "the static capability must stage the musl artifact"
    );
}

/// Every image now receives an already-built binary. Prove no Containerfile
/// still expects a staged workspace to compile from.
#[test]
fn containerfiles_install_a_staged_binary_without_building_from_source() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let containers = manifest_dir.join("../conary/tests/integration/remi/containers");
    for file in [
        "Containerfile.fedora44",
        "Containerfile.ubuntu-26.04",
        "Containerfile.arch",
        "Containerfile.artix",
        "Containerfile.debian-derivative",
    ] {
        let contents =
            fs::read_to_string(containers.join(file)).expect("read distro containerfile");
        assert!(
            contents.contains("install -m 755 /tmp/install/conary /usr/bin/conary"),
            "{file} must install the staged binary"
        );
        assert!(
            !contents.contains("source/"),
            "{file} must not stage the workspace source tree"
        );
        assert!(
            !contents.contains("cargo build"),
            "{file} must not build Conary from source"
        );
    }
}

#[test]
fn release_containerfiles_keep_a_separate_test_hook_binary() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let containers = manifest_dir.join("../conary/tests/integration/remi/containers");
    for file in [
        "Containerfile.fedora44",
        "Containerfile.ubuntu-26.04",
        "Containerfile.arch",
    ] {
        let contents =
            fs::read_to_string(containers.join(file)).expect("read release containerfile");
        assert!(
            contents.contains(
                "install -D -m 755 /tmp/install/conary \
                     /usr/libexec/conary-test/conary-test-hooks",
            ),
            "{file} must preserve the integration binary beside the published binary"
        );
    }
}

#[test]
fn artix_container_uses_core_mirrors_before_package_sync() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let containerfile =
        manifest_dir.join("../conary/tests/integration/remi/containers/Containerfile.artix");
    let contents = fs::read_to_string(containerfile).expect("read Artix containerfile");

    let mirror1 = contents
        .find("Server = https://mirror1.artixlinux.org/repos/$repo/os/$arch")
        .expect("Artix container must select the first official core mirror");
    let cvut = contents
        .find("Server = https://ftp.sh.cvut.cz/artix-linux/$repo/os/$arch")
        .expect("Artix container must select the second official core mirror");
    let sync = contents
        .find("pacman -Syyu --noconfirm")
        .expect("Artix container must force-refresh package databases before upgrading");

    assert!(
        mirror1 < sync && cvut < sync,
        "Artix core mirrors must be configured before package synchronization"
    );
    assert!(
        !contents.contains("--overwrite"),
        "the image must resolve repository coherence instead of masking file conflicts"
    );
}

#[test]
fn arch_container_uses_pinned_image_archive_before_package_sync() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let containerfile =
        manifest_dir.join("../conary/tests/integration/remi/containers/Containerfile.arch");
    let contents = fs::read_to_string(containerfile).expect("read Arch containerfile");

    let archive = contents
        .find("Server = https://archive.archlinux.org/repos/2026/08/02/$repo/os/$arch")
        .expect("Arch container must select the pinned image's official archive snapshot");
    let sync = contents
        .find("pacman -Syyu --noconfirm")
        .expect("Arch container must force-refresh archive package databases");

    assert!(
        archive < sync,
        "Arch archive authority must be configured before package synchronization"
    );
    assert!(
        contents.contains("version 20260802.0.566770"),
        "Arch archive date must remain visibly coupled to pinned image provenance"
    );
    assert!(
        contents.contains("pacman -Syyu --noconfirm --disable-download-timeout"),
        "Arch package synchronization must tolerate slow archive downloads"
    );
}

#[test]
fn package_mode_containerfiles_install_exact_canonical_artifacts() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let containers = manifest_dir.join("../conary/tests/integration/remi/containers");
    for (file, package, install) in [
        (
            "Containerfile.fedora44",
            "conary-release.rpm",
            "dnf install -y /tmp/install/conary-release.rpm",
        ),
        (
            "Containerfile.ubuntu-26.04",
            "conary-release.deb",
            "apt-get install -y /tmp/install/conary-release.deb",
        ),
        (
            "Containerfile.arch",
            "conary-release.pkg.tar.zst",
            "pacman -U --noconfirm /tmp/install/conary-release.pkg.tar.zst",
        ),
        (
            "Containerfile.artix",
            "conary-release.pkg.tar.zst",
            "pacman -U --noconfirm /tmp/install/conary-release.pkg.tar.zst",
        ),
        (
            "Containerfile.debian-derivative",
            "conary-release.deb",
            "apt-get install -y /tmp/install/conary-release.deb",
        ),
    ] {
        let contents =
            fs::read_to_string(containers.join(file)).expect("read package-mode containerfile");
        assert!(contents.contains(package), "{file} must name {package}");
        assert!(contents.contains(install), "{file} must run {install}");
    }
}

#[test]
fn native_package_staging_rejects_empty_files() {
    let directory = tempfile::tempdir().expect("create temp directory");
    let package = directory.path().join("empty.rpm");
    fs::write(&package, []).expect("write empty package");

    let error = stage_native_package(
        directory.path(),
        NativePackageArtifact {
            path: &package,
            format: ProfilePackageFormat::Rpm,
        },
    )
    .expect_err("empty package must fail");

    assert!(error.to_string().contains("must not be empty"));
}
