// apps/conary-test/src/container/image/static_shell.rs

//! Resolving, validating, and probing the hermetic static `/bin/sh` provider.

use anyhow::{Result, bail};
use std::fs;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use thiserror::Error;

use crate::config::TestManifest;
use crate::config::manifest::StaticFixture;

/// Whether any selected suite installs the hermetic `/bin/sh` provider fixture.
///
/// The provider's payload is a static shell copied from the host, so resolving
/// it must be a deliberate consequence of the manifests an image is built for,
/// not an unconditional image-build prerequisite. `NotInstalled` lets an image
/// build for suites that never install `conary-test-shell` without any host
/// shell present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellProviderRequirement {
    /// No selected suite installs the provider; no host shell is resolved.
    NotInstalled,
    /// A selected suite installs the provider, so its signed fixture is staged.
    Installed,
}

impl ShellProviderRequirement {
    /// Derive the requirement from each suite's typed `requires_fixtures` list.
    ///
    /// Command text is deliberately ignored: a quoted or built install command
    /// still opts an image into the provider by declaring the fixture, and a
    /// command that merely mentions the variable does not.
    pub fn from_manifests<'a>(manifests: impl IntoIterator<Item = &'a TestManifest>) -> Self {
        if manifests.into_iter().any(|manifest| {
            manifest
                .suite
                .requires_fixtures
                .contains(&StaticFixture::Shell)
        }) {
            Self::Installed
        } else {
            Self::NotInstalled
        }
    }

    pub(super) fn is_installed(&self) -> bool {
        matches!(self, Self::Installed)
    }
}

pub(super) const STATIC_TEST_SHELL_ENV: &str = "CONARY_TEST_STATIC_SHELL";

const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const ELF_CLASS_64: u8 = 2;
const ELF_DATA_LSB: u8 = 1;
const ELF_HEADER_LEN: usize = 64;
const ELF_PROGRAM_HEADER_LEN: usize = 56;
const ELF_DYNAMIC_ENTRY_LEN: usize = 16;
const E_TYPE_OFFSET: usize = 0x10;
const E_MACHINE_OFFSET: usize = 0x12;
const E_ENTRY_OFFSET: usize = 0x18;
const E_PHOFF_OFFSET: usize = 0x20;
const E_PHENTSIZE_OFFSET: usize = 0x36;
const E_PHNUM_OFFSET: usize = 0x38;
const P_FLAGS_OFFSET: usize = 4;
const P_OFFSET_OFFSET: usize = 8;
const P_VADDR_OFFSET: usize = 16;
const P_FILESZ_OFFSET: usize = 32;
const P_MEMSZ_OFFSET: usize = 40;
const PN_XNUM: u16 = 0xffff;

const ET_EXEC: u16 = 2;
const ET_DYN: u16 = 3;
const EM_X86_64: u16 = 0x3e;

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_INTERP: u32 = 3;

const PF_X: u32 = 0x1;

const DT_NULL: i64 = 0;
const DT_NEEDED: i64 = 1;

/// The architecture a distro test image runs, mapped to its ELF `e_machine`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImageArchitecture {
    X86_64,
}

impl ImageArchitecture {
    /// Every distro test image in this workspace targets x86_64.
    const IMAGE: Self = Self::X86_64;

    fn elf_machine(self) -> u16 {
        match self {
            Self::X86_64 => EM_X86_64,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
        }
    }
}

/// A typed reason a `conary-test-shell` candidate cannot serve as `/bin/sh`.
#[derive(Debug, Error)]
pub(super) enum StaticShellError {
    #[error("CONARY_TEST_STATIC_SHELL must name an existing executable regular file, got {path}")]
    ExplicitNotExecutable { path: PathBuf },
    #[error("static shell candidate {path} is not a little-endian 64-bit ELF binary")]
    NotElf64 { path: PathBuf },
    #[error(
        "static shell candidate {path} has ELF type {e_type:#06x}; expected ET_EXEC or static-PIE ET_DYN"
    )]
    UnsupportedType { path: PathBuf, e_type: u16 },
    #[error(
        "static shell candidate {path} targets ELF machine {machine:#06x}; expected {expected} ({expected_machine:#06x})"
    )]
    WrongMachine {
        path: PathBuf,
        machine: u16,
        expected: &'static str,
        expected_machine: u16,
    },
    #[error(
        "static shell candidate {path} is dynamically linked (it requests a program interpreter)"
    )]
    RequestsInterpreter { path: PathBuf },
    #[error(
        "static shell candidate {path} has DT_NEEDED dynamic dependencies; a fully static shell has none"
    )]
    HasDynamicDependencies { path: PathBuf },
    #[error(
        "static shell candidate {path} has no executable PT_LOAD segment with file content; the kernel would refuse to run it"
    )]
    NoExecutableLoadSegment { path: PathBuf },
    #[error(
        "static shell candidate {path} has entry point {entry:#x} outside every executable PT_LOAD segment"
    )]
    EntryPointOutsideExecutableLoad { path: PathBuf, entry: u64 },
    #[error(
        "static shell candidate {path} cannot run the fixture hook utilities (touch, rm): {reason}"
    )]
    MissingHookUtilities { path: PathBuf, reason: String },
    #[error("static shell candidate {path} has malformed ELF program headers: {reason}")]
    Malformed { path: PathBuf, reason: &'static str },
    #[error("static shell candidate {path} could not be read: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// Return true when `path` is a regular file with an execute bit set.
fn is_executable_regular_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }

    #[cfg(not(unix))]
    {
        true
    }
}

/// Locate the static shell the `conary-test-shell` fixture stages as `/bin/sh`.
///
/// An explicit `CONARY_TEST_STATIC_SHELL` wins and must validate. Otherwise the
/// first `busybox` on `PATH` that validates as a fully static ELF for the image
/// architecture is used; unusable candidates are skipped. Executability alone
/// is not enough: a dynamic busybox or a script would be signed as `/bin/sh`
/// and then fail inside the otherwise-empty selected-root chroot, so the ELF
/// is inspected and then functionally probed before staging.
pub(super) fn resolve_static_test_shell() -> Result<PathBuf> {
    if let Some(explicit) = std::env::var_os(STATIC_TEST_SHELL_ENV) {
        let path = PathBuf::from(explicit);
        if !is_executable_regular_file(&path) {
            return Err(StaticShellError::ExplicitNotExecutable { path }.into());
        }
        validate_static_shell(&path)?;
        probe_static_shell(&path)?;
        return Ok(path);
    }

    if let Some(path_var) = std::env::var_os("PATH") {
        let mut rejection = None;
        for directory in std::env::split_paths(&path_var) {
            let candidate = directory.join("busybox");
            if !is_executable_regular_file(&candidate) {
                continue;
            }
            match validate_static_shell(&candidate).and_then(|()| probe_static_shell(&candidate)) {
                Ok(()) => return Ok(candidate),
                Err(error) => {
                    tracing::debug!(
                        path = %candidate.display(),
                        error = %error,
                        "ignoring unusable busybox candidate"
                    );
                    rejection.get_or_insert(error);
                }
            }
        }
        if let Some(error) = rejection {
            return Err(error.into());
        }
    }

    bail!(
        "integration fixture conary-test-shell needs a statically linked shell: install busybox (static) or set {STATIC_TEST_SHELL_ENV}=<path>"
    )
}

/// Prove `path` is a fully static ELF the image architecture can execute.
pub(super) fn validate_static_shell(path: &Path) -> std::result::Result<(), StaticShellError> {
    let mut file = File::open(path).map_err(|source| StaticShellError::Read {
        path: path.to_path_buf(),
        source,
    })?;

    let mut header = [0u8; ELF_HEADER_LEN];
    file.read_exact(&mut header)
        .map_err(|_| StaticShellError::NotElf64 {
            path: path.to_path_buf(),
        })?;

    if header[..4] != ELF_MAGIC || header[4] != ELF_CLASS_64 || header[5] != ELF_DATA_LSB {
        return Err(StaticShellError::NotElf64 {
            path: path.to_path_buf(),
        });
    }

    let e_type = read_u16(&header, E_TYPE_OFFSET);
    let e_machine = read_u16(&header, E_MACHINE_OFFSET);
    let e_entry = read_u64(&header, E_ENTRY_OFFSET);
    let e_phoff = read_u64(&header, E_PHOFF_OFFSET);
    let e_phentsize = read_u16(&header, E_PHENTSIZE_OFFSET);
    let e_phnum = read_u16(&header, E_PHNUM_OFFSET);

    if !matches!(e_type, ET_EXEC | ET_DYN) {
        return Err(StaticShellError::UnsupportedType {
            path: path.to_path_buf(),
            e_type,
        });
    }

    let expected = ImageArchitecture::IMAGE;
    if e_machine != expected.elf_machine() {
        return Err(StaticShellError::WrongMachine {
            path: path.to_path_buf(),
            machine: e_machine,
            expected: expected.label(),
            expected_machine: expected.elf_machine(),
        });
    }

    if e_phnum == PN_XNUM {
        return Err(StaticShellError::Malformed {
            path: path.to_path_buf(),
            reason: "program header count is stored in the section table (PN_XNUM)",
        });
    }
    if e_phnum == 0 || usize::from(e_phentsize) < ELF_PROGRAM_HEADER_LEN {
        return Err(StaticShellError::Malformed {
            path: path.to_path_buf(),
            reason: "missing usable ELF64 program headers",
        });
    }

    let mut requests_interpreter = false;
    let mut dynamic_segment = None;
    let mut executable_load_segments = Vec::new();
    let mut program_header = vec![0u8; usize::from(e_phentsize)];

    for index in 0..u64::from(e_phnum) {
        let offset = e_phoff + index * u64::from(e_phentsize);
        file.seek(SeekFrom::Start(offset))
            .map_err(|source| StaticShellError::Read {
                path: path.to_path_buf(),
                source,
            })?;
        file.read_exact(&mut program_header)
            .map_err(|_| StaticShellError::Malformed {
                path: path.to_path_buf(),
                reason: "program header table is truncated",
            })?;

        let p_type = read_u32(&program_header, 0);
        match p_type {
            PT_LOAD => {
                let p_flags = read_u32(&program_header, P_FLAGS_OFFSET);
                let p_filesz = read_u64(&program_header, P_FILESZ_OFFSET);
                // A load segment with no file content cannot hold code, and
                // without execute permission the kernel refuses the image.
                if p_flags & PF_X != 0 && p_filesz > 0 {
                    executable_load_segments.push((
                        read_u64(&program_header, P_VADDR_OFFSET),
                        read_u64(&program_header, P_MEMSZ_OFFSET),
                    ));
                }
            }
            PT_INTERP => requests_interpreter = true,
            PT_DYNAMIC => {
                dynamic_segment = Some((
                    read_u64(&program_header, P_OFFSET_OFFSET),
                    read_u64(&program_header, P_FILESZ_OFFSET),
                ));
            }
            _ => {}
        }
    }

    if requests_interpreter {
        return Err(StaticShellError::RequestsInterpreter {
            path: path.to_path_buf(),
        });
    }

    if let Some((offset, size)) = dynamic_segment
        && dynamic_table_has_needed(&mut file, path, offset, size)?
    {
        return Err(StaticShellError::HasDynamicDependencies {
            path: path.to_path_buf(),
        });
    }

    if executable_load_segments.is_empty() {
        return Err(StaticShellError::NoExecutableLoadSegment {
            path: path.to_path_buf(),
        });
    }

    // For both `ET_EXEC` and static-PIE `ET_DYN`, the entry point is a virtual
    // address the kernel jumps to, so it must fall inside an executable
    // `PT_LOAD` or the process dies before `main`.
    let entry_is_executable = executable_load_segments.iter().any(|(vaddr, memsz)| {
        let end = vaddr.saturating_add(*memsz);
        e_entry >= *vaddr && e_entry < end
    });
    if !entry_is_executable {
        return Err(StaticShellError::EntryPointOutsideExecutableLoad {
            path: path.to_path_buf(),
            entry: e_entry,
        });
    }

    Ok(())
}

/// Return true when a `PT_DYNAMIC` table carries a `DT_NEEDED` entry.
fn dynamic_table_has_needed(
    file: &mut File,
    path: &Path,
    offset: u64,
    size: u64,
) -> std::result::Result<bool, StaticShellError> {
    if size == 0 || !size.is_multiple_of(ELF_DYNAMIC_ENTRY_LEN as u64) {
        return Err(StaticShellError::Malformed {
            path: path.to_path_buf(),
            reason: "PT_DYNAMIC size is not a whole number of ELF64 dynamic entries",
        });
    }

    file.seek(SeekFrom::Start(offset))
        .map_err(|source| StaticShellError::Read {
            path: path.to_path_buf(),
            source,
        })?;

    let mut entry = [0u8; ELF_DYNAMIC_ENTRY_LEN];
    for _ in 0..size / ELF_DYNAMIC_ENTRY_LEN as u64 {
        file.read_exact(&mut entry)
            .map_err(|_| StaticShellError::Malformed {
                path: path.to_path_buf(),
                reason: "PT_DYNAMIC table is truncated",
            })?;
        match i64::from_le_bytes(entry[..8].try_into().expect("8 bytes")) {
            DT_NULL => return Ok(false),
            DT_NEEDED => return Ok(true),
            _ => {}
        }
    }

    Ok(false)
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().expect("2 bytes"))
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("4 bytes"))
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("8 bytes"))
}

/// How long the functional shell probe may run before the candidate is refused.
const STATIC_SHELL_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Prove `path` can execute the fixture hooks' external commands (`touch`, `rm`)
/// in a cleared environment.
///
/// `/bin/sh` runs in an otherwise empty selected-root chroot with no coreutils,
/// so a shell that merely parses commands is not enough. The candidate is
/// dispatched as `argv[0] = "sh"` (busybox selects its shell by that name) with
/// a cleared environment whose `PATH` is an empty directory, then asked to run
/// the exact external command set the `conary-test-fixture` hooks use:
/// `touch <file> && rm <file>`. A shell that cannot run either utility, or that
/// leaves the probe file behind, is refused.
pub(super) fn probe_static_shell(path: &Path) -> std::result::Result<(), StaticShellError> {
    let refuse = |reason: String| StaticShellError::MissingHookUtilities {
        path: path.to_path_buf(),
        reason,
    };

    let work_dir = tempfile::tempdir()
        .map_err(|source| refuse(format!("could not create the probe directory: {source}")))?;
    // `PATH` points at a second, empty directory so the candidate cannot find
    // `touch` or `rm` on the host.
    let empty_path = tempfile::tempdir()
        .map_err(|source| refuse(format!("could not create the empty PATH: {source}")))?;

    let mut command = std::process::Command::new(path);
    command
        .arg0("sh")
        .arg("-c")
        .arg("touch probe && rm probe")
        .current_dir(work_dir.path())
        .env_clear()
        .env("PATH", empty_path.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    let mut child = command
        .spawn()
        .map_err(|source| refuse(format!("could not execute: {source}")))?;

    let deadline = Instant::now() + STATIC_SHELL_PROBE_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(refuse(format!(
                        "did not finish within {}s",
                        STATIC_SHELL_PROBE_TIMEOUT.as_secs()
                    )));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(source) => {
                return Err(refuse(format!(
                    "failed while waiting for the probe: {source}"
                )));
            }
        }
    };

    if !status.success() {
        return Err(refuse(format!(
            "exited with {status} instead of running touch and rm"
        )));
    }
    if work_dir.path().join("probe").exists() {
        return Err(refuse(
            "left the probe file behind instead of running rm".to_string(),
        ));
    }

    Ok(())
}
