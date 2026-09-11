// crates/conary-xtask/src/line_cap/exemption.rs

//! How an exempt-named file earns its exemption from the line caps.
//!
//! `line_cap` decides *whether* an exempt-named file is skipped; this module
//! decides whether that skip is defensible, by resolving module and `include!`
//! declarations the way Rust loads them and walking the resulting graph to a
//! `#[cfg(test)]` gate or a cargo test target. The gate itself stays hermetic
//! and report-only: classification never changes a cap outcome.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{Attribute, Expr, ExprLit, Item, ItemMod, Lit, Meta};

use super::cfg;
use super::{cfg_attributes, item_attributes, path_text};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum ExemptionGate {
    /// Defensibly test-only. Established by any of: an inner `#![cfg(test)]` on
    /// the file itself; a cargo integration-test target root; or every module
    /// declaration that resolves to the file (transitively) gated by
    /// `#[cfg(test)]`.
    TestGated,
    /// No gate is established anywhere, so the file's name alone hides it from
    /// the caps. This is the masking case issue #997 exposes.
    Ungated,
}

impl ExemptionGate {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::TestGated => "test-gated",
            Self::Ungated => "ungated",
        }
    }
}

pub(crate) fn exemption_gate(test_gated: bool) -> ExemptionGate {
    if test_gated {
        ExemptionGate::TestGated
    } else {
        ExemptionGate::Ungated
    }
}

/// A `mod x;` declaration that makes one file a module of another.
#[derive(Debug)]
pub(crate) struct ModuleDeclaration {
    declaring_file: String,
    /// The declaration's own gate: its `cfg`/`cfg_attr` attributes conjoined
    /// with those of every enclosing inline module.
    test_gated: bool,
}

/// The file's own gate, before any declaring site is considered.
pub(crate) fn file_level_test_gate(syntax: &syn::File, relative: &Path) -> bool {
    cfg::is_test_only(&syntax.attrs) || cargo_target_root(relative) == Some(CargoTarget::Test)
}

/// Index every external module declaration by the repo-relative path Rust would
/// load for it, so exempt-named files can be classified by their declaring
/// sites rather than by their own text.
pub(crate) fn collect_module_declarations(
    syntax: &syn::File,
    relative: &Path,
    declarations: &mut BTreeMap<String, Vec<ModuleDeclaration>>,
) {
    let mut collector = DeclarationCollector {
        declaring_file: path_text(relative),
        file_dir: relative
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .to_path_buf(),
        declarations,
    };
    let module_dir = relative_module_directory(relative);
    collector.run(&syntax.items, &module_dir, &syntax.attrs);
}

pub(crate) struct DeclarationCollector<'a> {
    declaring_file: String,
    /// Directory of the containing file, which is the base for `#[path]` on a
    /// declaration that is not nested in an inline module. Verified against
    /// `apps/remi/src/server/catalog_authority.rs`, whose
    /// `#[path = "catalog_authority/tests/test_support.rs"]` resolves below
    /// `apps/remi/src/server/`, and against `apps/conary/build.rs`, whose
    /// `#[path = "src/cli/mod.rs"]` resolves below `apps/conary/`.
    file_dir: PathBuf,
    declarations: &'a mut BTreeMap<String, Vec<ModuleDeclaration>>,
}

impl DeclarationCollector<'_> {
    fn run(&mut self, items: &[Item], module_dir: &Path, file_attributes: &[Attribute]) {
        let path_base = self.file_dir.clone();
        self.collect(items, module_dir, &path_base, file_attributes);
    }

    fn collect(
        &mut self,
        items: &[Item],
        module_dir: &Path,
        path_base: &Path,
        inherited: &[Attribute],
    ) {
        for item in items {
            let Item::Mod(item_module) = item else {
                continue;
            };
            let mut effective = inherited.to_vec();
            effective.extend(cfg_attributes(&item_module.attrs));
            let test_gated = cfg::is_test_only(&effective);
            let name = item_module.ident.to_string();
            let declared_path = path_attribute(&item_module.attrs);

            if let Some((_, items)) = &item_module.content {
                // An inline module owns a directory named after it; a `#[path]`
                // attribute on it renames that directory.
                let nested_dir = match &declared_path {
                    Some(path) => module_dir.join(path),
                    None => module_dir.join(&name),
                };
                self.collect(items, &nested_dir, &nested_dir, &effective);
                continue;
            }

            match declared_path {
                Some(path) => self.record(normalize(&path_base.join(path)), test_gated),
                None => {
                    // Rust loads `<module>/<name>.rs` or, failing that,
                    // `<module>/<name>/mod.rs`.
                    self.record(module_dir.join(format!("{name}.rs")), test_gated);
                    self.record(module_dir.join(&name).join("mod.rs"), test_gated);
                }
            }
        }
    }

    fn record(&mut self, target: PathBuf, test_gated: bool) {
        self.declarations
            .entry(path_text(&target))
            .or_default()
            .push(ModuleDeclaration {
                declaring_file: self.declaring_file.clone(),
                test_gated,
            });
    }
}

/// Index textual `include!("x.rs")` sites as declaring sites too. An included
/// file is not a module, but its tokens are compiled in the including module, so
/// a `#[cfg(test)]` include site is exactly as strong as a gated declaration.
/// This is how `crates/conary-core/src/repository/sync/tests.rs` and its
/// `tests/native.rs` chain are compiled: `sync.rs` ends with
/// `#[cfg(test)] include!("sync/tests.rs");`.
pub(crate) fn collect_include_declarations(
    syntax: &syn::File,
    relative: &Path,
    declarations: &mut BTreeMap<String, Vec<ModuleDeclaration>>,
) {
    let mut visitor = IncludeVisitor {
        declaring_file: path_text(relative),
        file_dir: relative
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .to_path_buf(),
        inherited: Vec::new(),
        declarations,
    };
    visitor.visit_file(syntax);
}

pub(crate) struct IncludeVisitor<'a> {
    declaring_file: String,
    /// The included path is relative to the containing file's directory, the
    /// same base `#[path]` uses for a declaration outside an inline module.
    file_dir: PathBuf,
    inherited: Vec<Attribute>,
    declarations: &'a mut BTreeMap<String, Vec<ModuleDeclaration>>,
}

impl IncludeVisitor<'_> {
    fn record(&mut self, attributes: &[Attribute], mac: &syn::Macro) {
        if !mac.path.is_ident("include") {
            return;
        }
        let Ok(path) = syn::parse2::<syn::LitStr>(mac.tokens.clone()) else {
            return;
        };
        let mut effective = self.inherited.clone();
        effective.extend(cfg_attributes(attributes));
        let target = normalize(&self.file_dir.join(path.value()));
        self.declarations
            .entry(path_text(&target))
            .or_default()
            .push(ModuleDeclaration {
                declaring_file: self.declaring_file.clone(),
                test_gated: cfg::is_test_only(&effective),
            });
    }
}

impl<'ast> Visit<'ast> for IncludeVisitor<'_> {
    fn visit_item(&mut self, item: &'ast Item) {
        let depth = self.inherited.len();
        self.inherited.extend(cfg_attributes(item_attributes(item)));
        visit::visit_item(self, item);
        self.inherited.truncate(depth);
    }

    fn visit_item_macro(&mut self, node: &'ast syn::ItemMacro) {
        self.record(&node.attrs, &node.mac);
    }

    fn visit_stmt_macro(&mut self, node: &'ast syn::StmtMacro) {
        self.record(&node.attrs, &node.mac);
    }

    fn visit_expr_macro(&mut self, node: &'ast syn::ExprMacro) {
        self.record(&node.attrs, &node.mac);
    }
}

/// Propagate file-level gates through the module graph until it stabilizes. A
/// file is test-only when its own attributes gate it, or when it is declared at
/// least once and every declaration resolving to it is test-gated (directly or
/// because the declaring file is itself test-only). A single ungated declaring
/// site keeps the file in production builds.
pub(crate) fn resolve_test_gates(
    file_gates: &BTreeMap<String, bool>,
    declarations: &BTreeMap<String, Vec<ModuleDeclaration>>,
) -> BTreeMap<String, bool> {
    let mut gated = file_gates.clone();
    loop {
        let mut changed = false;
        for (target, sites) in declarations {
            if gated.get(target).copied().unwrap_or(false) {
                continue;
            }
            let all_sites_gated = sites.iter().all(|site| {
                site.test_gated || gated.get(&site.declaring_file).copied().unwrap_or(false)
            });
            if all_sites_gated {
                gated.insert(target.clone(), true);
                changed = true;
            }
        }
        if !changed {
            return gated;
        }
    }
}

/// How cargo compiles a file, when the file is a target root.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum CargoTarget {
    /// An integration test target (`<package>/tests/<name>.rs`). Cargo builds it
    /// only for `cargo test`, so the whole module tree below it is test-only.
    Test,
    /// A bin, bench, or example target root: a crate root that owns its
    /// directory, but is not test-only.
    Other,
}

/// Cargo target roots relative to `apps/<package>/` or `crates/<package>/`.
/// Auto-discovery covers `<dir>/<name>.rs` and `<dir>/<name>/main.rs` below
/// `tests/`, `benches/`, `examples/`, and `src/bin/`.
pub(crate) fn cargo_target_root(relative: &Path) -> Option<CargoTarget> {
    let rest = package_relative(relative)?;
    let parts = rest
        .components()
        .filter_map(normal_text)
        .collect::<Vec<String>>();
    let (kind, tail) = match parts.split_first()?.0.as_str() {
        "tests" => (CargoTarget::Test, &parts[1..]),
        "benches" | "examples" => (CargoTarget::Other, &parts[1..]),
        "src" => match parts.get(1).map(String::as_str) {
            Some("bin") => (CargoTarget::Other, &parts[2..]),
            _ => return None,
        },
        _ => return None,
    };
    let is_root = match tail {
        [name] => name.ends_with(".rs"),
        [_, main] => main == "main.rs",
        _ => false,
    };
    is_root.then_some(kind)
}

pub(crate) fn package_relative(relative: &Path) -> Option<PathBuf> {
    let mut components = relative.components();
    let Component::Normal(kind) = components.next()? else {
        return None;
    };
    if kind != OsStr::new("apps") && kind != OsStr::new("crates") {
        return None;
    }
    components.next()?;
    Some(components.as_path().to_path_buf())
}

pub(crate) fn normal_text(component: Component<'_>) -> Option<String> {
    match component {
        Component::Normal(text) => Some(text.to_string_lossy().into_owned()),
        _ => None,
    }
}

/// The directory Rust searches for `mod x;` declared by this repo-relative
/// path, as a path relative to the scan root. Distinct from
/// `module_directory`, which answers the same question for an absolute
/// containing path and reports `None` when the stem is missing.
pub(crate) fn relative_module_directory(relative: &Path) -> PathBuf {
    let parent = relative.parent().unwrap_or_else(|| Path::new(""));
    if owns_its_directory(relative) {
        parent.to_path_buf()
    } else {
        parent.join(relative.file_stem().unwrap_or(OsStr::new("")))
    }
}

/// Crate roots and `mod.rs` own their containing directory; any other file owns
/// a sibling directory named after its file stem. `Foo.rs` therefore declares
/// `mod tests;` as `Foo/tests.rs` while `Foo/mod.rs` declares it as
/// `Foo/tests.rs` too; both forms are verified in unit tests and against
/// `apps/conary-test/src/bootstrap.rs` (`bootstrap/tests.rs`) and
/// `apps/conary-test/src/config/mod.rs` (`config/tests.rs`).
pub(crate) fn owns_its_directory(relative: &Path) -> bool {
    let name = relative.file_name().and_then(OsStr::to_str);
    matches!(name, Some("mod.rs" | "lib.rs" | "main.rs" | "build.rs"))
        || cargo_target_root(relative).is_some()
}

pub(crate) fn path_attribute(attributes: &[Attribute]) -> Option<String> {
    attributes.iter().find_map(|attribute| {
        if !attribute.path().is_ident("path") {
            return None;
        }
        let Meta::NameValue(named) = &attribute.meta else {
            return None;
        };
        let Expr::Lit(literal) = &named.value else {
            return None;
        };
        let Lit::Str(value) = &literal.lit else {
            return None;
        };
        Some(value.value())
    })
}

/// Resolve `.` and `..` components of a `#[path]` value without touching the
/// filesystem, so `../../tests/common/update_ccs.rs` from
/// `apps/conary/src/commands/test_helpers.rs` becomes
/// `apps/conary/tests/common/update_ccs.rs`.
pub(crate) fn normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}
