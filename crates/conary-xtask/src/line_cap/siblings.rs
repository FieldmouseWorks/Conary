// crates/conary-xtask/src/line_cap/siblings.rs

//! Attribute extracted sibling test mass to its declaring parent.
//!
//! The cap policy requires a sibling extraction to reduce the parent "with that
//! reduction stated in the commit". That statement is unverifiable if the tool
//! cannot see the relation, so this module resolves each top-level `mod`
//! declaration to the file Rust would load and reports the sibling's measured
//! size on the parent's row.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

use syn::{Attribute, Expr, ExprLit, Item, ItemMod, Lit, Meta};

use super::{FileMetrics, MeasuredFiles, SiblingAttribution};

pub(crate) fn report_row(
    relative: &str,
    metrics: FileMetrics,
    attribution: SiblingAttribution,
) -> String {
    let mut row = format!(
        "{relative}\ttotal={}\tproduction={}\tinline_test={}",
        metrics.total_lines, metrics.production_lines, metrics.inline_test_lines
    );
    if attribution.siblings > 0 {
        row.push_str(&format!(
            "\tsiblings={}\tsibling_tests={}\treduction={}",
            attribution.siblings, attribution.sibling_lines, attribution.sibling_lines
        ));
    }
    row
}

/// Sum the measured mass of a file's resolved child modules. A child that
/// cannot be read or parsed is left out of both the count and the sum; it is
/// already reported as its own scan error.
pub(crate) fn sibling_attribution(
    children: &[PathBuf],
    measured: &mut MeasuredFiles,
) -> SiblingAttribution {
    let mut attribution = SiblingAttribution::default();
    for child in children {
        let Some(metrics) = measured.measure(child) else {
            continue;
        };
        attribution.siblings += 1;
        attribution.sibling_lines += metrics.total_lines;
    }
    attribution
}

/// Resolve every top-level `mod name;` declaration (one with no inline body) to
/// the file rustc would read for it, keeping only files inside the scanned
/// roots. Declarations naming no scanned file are dropped rather than guessed
/// at.
///
/// The declaration is attributed to the file that declares it, so a parent that
/// shrank by moving tests into `<parent-stem>/tests.rs` now states the mass that
/// left it and the exempt sibling reports itself on an `EXTRACTED:` line.
///
/// Known limit: `lib.rs`, `main.rs`, and `build.rs` are crate roots and really
/// own their own directory, but they are treated like any other non-`mod.rs`
/// file here. Crate roots carry ordinary `mod` wiring rather than extracted
/// test siblings, so leaving them unresolved under-reports instead of
/// mis-attributing production modules as a test reduction.
pub(crate) fn resolve_child_modules(
    containing: &Path,
    source: &str,
    scanned: &BTreeSet<PathBuf>,
) -> Vec<PathBuf> {
    let Ok(syntax) = syn::parse_file(source) else {
        return Vec::new();
    };
    let mut resolved = BTreeSet::new();
    for item in &syntax.items {
        let Item::Mod(declaration) = item else {
            continue;
        };
        if declaration.content.is_some() {
            continue;
        }
        let candidate = match declared_module_path(declaration) {
            // rustc resolves `#[path]` against the directory of the containing
            // file, not against the module directory chosen below.
            Some(relative) => containing
                .parent()
                .map(|directory| normalize_path(directory.join(relative))),
            None => module_directory(containing).and_then(|directory| {
                let name = declaration.ident.to_string();
                [
                    directory.join(format!("{name}.rs")),
                    directory.join(&name).join("mod.rs"),
                ]
                .into_iter()
                .find(|candidate| scanned.contains(candidate))
            }),
        };
        if let Some(candidate) = candidate
            && scanned.contains(&candidate)
        {
            resolved.insert(candidate);
        }
    }
    resolved.into_iter().collect()
}

/// The directory rustc searches for a child declared without `#[path]`. A
/// `mod.rs` file owns the directory it sits in; every other file owns
/// `<dir>/<stem>/`, which is why `canonical.rs` declares `mod tests;` as
/// `canonical/tests.rs`.
pub(crate) fn module_directory(containing: &Path) -> Option<PathBuf> {
    let directory = containing.parent()?;
    if containing.file_name() == Some(OsStr::new("mod.rs")) {
        return Some(directory.to_path_buf());
    }
    Some(directory.join(containing.file_stem()?))
}

/// The literal `#[path = "..."]` value on a `mod` declaration, when present.
pub(crate) fn declared_module_path(declaration: &ItemMod) -> Option<String> {
    declaration.attrs.iter().find_map(|attribute| {
        if !attribute.path().is_ident("path") {
            return None;
        }
        let Meta::NameValue(name_value) = &attribute.meta else {
            return None;
        };
        let Expr::Lit(ExprLit {
            lit: Lit::Str(text),
            ..
        }) = &name_value.value
        else {
            return None;
        };
        Some(text.value())
    })
}

/// Lexically resolve `.` and `..` so a joined `#[path]` compares equal to the
/// paths collected from the scan without touching the filesystem.
pub(crate) fn normalize_path(path: PathBuf) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    normalized
}
