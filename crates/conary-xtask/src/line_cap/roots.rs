// crates/conary-xtask/src/line_cap/roots.rs

//! Which top-level Rust source roots the cap gate scans, and under what policy.
//!
//! A root is either measured, or excluded as vendored upstream source with a
//! stated reason. A top-level directory holding `.rs` files that no policy
//! declares fails the gate, so a new source root cannot escape measurement
//! silently.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

/// How a declared top-level Rust source root participates in the cap gate.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum RootPolicy {
    /// Measured: path comments validated, both caps compared, allowlist-eligible.
    Scanned,
    /// Vendored upstream source patched into the build by path; not
    /// Conary-authored. Deliberately not measured, and therefore not
    /// allowlist-eligible.
    VendorExcluded { reason: &'static str },
}

impl RootPolicy {
    fn is_scanned(self) -> bool {
        matches!(self, Self::Scanned)
    }
}

/// A declared top-level Rust source root and its cap-gate policy.
pub(crate) struct SourceRoot {
    pub(crate) name: &'static str,
    pub(crate) policy: RootPolicy,
}

/// Every top-level directory that holds Rust built into the product, with the
/// policy the cap gate applies to it. A top-level directory carrying `.rs`
/// files that is absent here fails the gate until its policy is recorded, so a
/// new source root cannot escape measurement silently.
pub(crate) const SOURCE_ROOTS: &[SourceRoot] = &[
    SourceRoot {
        name: "apps",
        policy: RootPolicy::Scanned,
    },
    SourceRoot {
        name: "crates",
        policy: RootPolicy::Scanned,
    },
    SourceRoot {
        name: "third_party",
        policy: RootPolicy::VendorExcluded {
            reason: "vendored aws-creds, rust-s3, and resolvo patched by path through [patch.crates-io] in Cargo.toml",
        },
    },
];

/// Directory names that never hold a scannable Rust source root: build output,
/// dependency trees, and VCS metadata. Directories with a leading `.` are
/// ignored by name as well, at the top level and inside a scan candidate.
pub(crate) const IGNORED_DIRECTORY_NAMES: &[&str] =
    &["target", "node_modules", ".git", ".worktrees"];

/// One declared root's measured contribution to the coverage statement.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct RootCoverage {
    pub(crate) name: &'static str,
    pub(crate) policy: RootPolicy,
    pub(crate) files: usize,
}

/// How many files each declared root contributed, plus the files the gate
/// measures. Vendor-excluded roots are counted but never measured.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct SourceScan {
    pub(crate) files: Vec<PathBuf>,
    pub(crate) coverage: Vec<RootCoverage>,
}

pub(crate) fn declared_source_root(name: &str) -> Option<&'static SourceRoot> {
    SOURCE_ROOTS
        .iter()
        .find(|source_root| source_root.name == name)
}

pub(crate) fn skipped_directory(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    name.starts_with('.') || IGNORED_DIRECTORY_NAMES.contains(&name)
}

pub(crate) fn rust_source_files(root: &Path) -> Result<SourceScan, String> {
    let scan = scan_source_roots(root)?;
    let undeclared = undeclared_rust_roots(root)?;
    if undeclared.is_empty() {
        return Ok(scan);
    }
    Err(format!(
        "undeclared top-level Rust source root(s) below {}: {}; classify each in SOURCE_ROOTS in crates/conary-xtask/src/line_cap.rs as Scanned or VendorExcluded",
        root.display(),
        undeclared.join(", ")
    ))
}

pub(crate) fn scan_source_roots(root: &Path) -> Result<SourceScan, String> {
    let mut files = Vec::new();
    let mut coverage = Vec::new();
    let mut scanned_roots = Vec::new();
    for source_root in SOURCE_ROOTS {
        let directory = root.join(source_root.name);
        let mut root_files = Vec::new();
        if directory.is_dir() {
            collect_rust_files(&directory, &mut root_files)?;
        }
        coverage.push(RootCoverage {
            name: source_root.name,
            policy: source_root.policy,
            files: root_files.len(),
        });
        if source_root.policy.is_scanned() && directory.is_dir() {
            scanned_roots.push(source_root.name);
            files.extend(root_files);
        }
    }
    if scanned_roots.is_empty() {
        return Err(format!(
            "no scanned source root below {} (declared: {})",
            root.display(),
            SOURCE_ROOTS
                .iter()
                .filter(|source_root| source_root.policy.is_scanned())
                .map(|source_root| source_root.name)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    files.sort();
    Ok(SourceScan { files, coverage })
}

/// The `--report` coverage statement: one entry per declared root naming the
/// filesystem-derived file count and the policy applied to it.
pub(crate) fn source_roots_text(coverage: &[RootCoverage]) -> String {
    coverage
        .iter()
        .map(|covered| match covered.policy {
            RootPolicy::Scanned => format!("{}={} files (scanned)", covered.name, covered.files),
            RootPolicy::VendorExcluded { reason } => format!(
                "{}={} files (vendor-excluded: {reason})",
                covered.name, covered.files
            ),
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Top-level directories below `root` that carry `.rs` files but have no
/// recorded policy. Each candidate is probed with a walk that stops at the
/// first `.rs` file instead of enumerating the whole tree.
pub(crate) fn undeclared_rust_roots(root: &Path) -> Result<Vec<String>, String> {
    let entries =
        fs::read_dir(root).map_err(|error| format!("cannot read {}: {error}", root.display()))?;
    let mut undeclared = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot read directory entry: {error}"))?;
        let file_name = entry.file_name();
        if skipped_directory(&file_name) {
            continue;
        }
        let name = file_name.to_string_lossy();
        if declared_source_root(name.as_ref()).is_some() {
            continue;
        }
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
        if file_type.is_dir() && contains_rust_file(&path)? {
            undeclared.push(name.into_owned());
        }
    }
    undeclared.sort();
    Ok(undeclared)
}

/// Whether a `.rs` file exists anywhere below `directory`, stopping at the
/// first match.
pub(crate) fn contains_rust_file(directory: &Path) -> Result<bool, String> {
    let entries = fs::read_dir(directory)
        .map_err(|error| format!("cannot read {}: {error}", directory.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot read directory entry: {error}"))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
        if file_type.is_file() && path.extension() == Some(OsStr::new("rs")) {
            return Ok(true);
        }
        if file_type.is_dir()
            && !skipped_directory(entry.file_name().as_os_str())
            && contains_rust_file(&path)?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn collect_rust_files(directory: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(directory)
        .map_err(|error| format!("cannot read {}: {error}", directory.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot read directory entry: {error}"))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
        if file_type.is_dir() {
            if entry.file_name() != OsStr::new("target") {
                collect_rust_files(&path, files)?;
            }
        } else if file_type.is_file() && path.extension() == Some(OsStr::new("rs")) {
            files.push(path);
        }
    }
    Ok(())
}
