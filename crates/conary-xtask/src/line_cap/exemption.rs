// crates/conary-xtask/src/line_cap/exemption.rs

//! How an exempt-named file earns its exemption from the line caps.
//!
//! `line_cap` decides *whether* an exempt-named file is skipped; this module
//! decides how that file is compiled, by resolving `mod` and `include!`
//! declarations the way rustc loads them and walking the resulting graph to a
//! `#[cfg(test)]` gate or a cargo test target. It owns both the declaration
//! graph and the gate resolution so `siblings` attributes extracted test mass
//! through the same reasoning instead of a second, disagreeing rule.
//!
//! Classification is report-only and never changes a cap outcome. Enforcing a
//! cap on an exempt-named file stays deferred, which is also why a listed
//! exempt-named file is stale however large it measures: the name, not the
//! allowlist, is what keeps it out of the caps, so there is no violation for an
//! exception to excuse. Only a cap-checked file can make an entry used.
//!
//! Three answers are possible, and the third is deliberate. An inner
//! `#![cfg(test)]` is intrinsic, so it is hard: no declaration can un-gate it.
//! Cargo test-target membership is only a *context* — it says the file is
//! compiled as a test target, not that every use of it is test-only — so a
//! declaring site that is not test-gated, from a file that is itself
//! production, keeps the file out of the test-only set. When neither answer is
//! established the file stays [`ExemptionGate::Unknown`] rather than being
//! guessed into one.
//!
//! Deliberate limits. Cargo target discovery is by path convention
//! (`<package>/tests/*.rs` and friends) and does not read the Cargo manifest,
//! so a customized target path or an `autotests = false` layout is invisible
//! here. `mod` declarations produced by macros, and `include!` calls whose
//! argument is not a literal, are not seen at all. A file that no seen
//! declaration reaches, and that no crate-root convention explains, stays
//! `unknown`.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

use syn::visit::{self, Visit};
use syn::{Attribute, Expr, Item, Lit, Meta};

use super::cfg;
use super::{FileMetrics, cfg_attributes, item_attributes, path_text};

/// How an exempt-named file is compiled, as far as the declaration graph can
/// establish it.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum ExemptionGate {
    /// Defensibly test-only: an inner `#![cfg(test)]`, or every declaring site
    /// test-gated (directly, or through a declaring file that is test-only
    /// itself), or a cargo integration-test target no other site reaches.
    TestGated,
    /// A non-test compilation reaches the file: a declaring site that is not
    /// test-gated, declared by a file that is itself production, or a crate
    /// root that is not a test target.
    Ungated,
    /// The analysis establishes neither: nothing declares the file, it carries
    /// no intrinsic gate, and no cargo target convention explains it.
    Unknown,
}

impl ExemptionGate {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::TestGated => "test-gated",
            Self::Ungated => "ungated",
            Self::Unknown => "unknown",
        }
    }
}

/// The gate a file's resolved state carries, defaulting to `unknown` for a file
/// the resolution never saw.
pub(crate) fn resolved_gate(
    gates: &BTreeMap<String, ExemptionGate>,
    relative: &str,
) -> ExemptionGate {
    gates
        .get(relative)
        .copied()
        .unwrap_or(ExemptionGate::Unknown)
}

/// The predicate `line_cap` skips files by: a file named exactly `tests.rs`, or
/// one below a path component named `tests`.
pub(crate) fn excluded_test_file(relative: &Path) -> bool {
    relative.file_name() == Some(OsStr::new("tests.rs"))
        || relative
            .components()
            .any(|component| component == Component::Normal(OsStr::new("tests")))
}

/// How a declaration names its target, which decides how `resolve_child_modules`
/// chooses between candidates.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum DeclarationKind {
    /// `#[path = "..."]` or `include!("...")`: the target is exact.
    Exact,
    /// `mod name;` resolved to `<directory>/<name>.rs`.
    Flat,
    /// `mod name;` resolved to `<directory>/<name>/mod.rs`.
    ModuleRoot,
}

/// One declaring site that makes a file part of another file's compilation.
#[derive(Debug)]
pub(crate) struct ModuleDeclaration {
    declaring_file: String,
    /// The declared name for `mod name;`, which pairs the flat and `mod.rs`
    /// candidates; `None` for `#[path]` and `include!`, whose target is exact.
    name: Option<String>,
    kind: DeclarationKind,
    /// How many inline modules enclose the declaration. Only a top-level
    /// declaration names a child the parent's own row attributes.
    depth: usize,
    /// The declaration's own gate: its `cfg`/`cfg_attr` attributes conjoined
    /// with those of every enclosing inline module.
    test_gated: bool,
    /// True for a `mod` declaration, false for an `include!` site. An included
    /// file is compiled as part of the includer, so it gates the same way but
    /// is not a module sibling.
    is_module: bool,
}

/// Index every external module declaration by the repo-relative path rustc
/// would load for it, so a file can be classified by its declaring sites rather
/// than by its own text.
pub(crate) fn collect_module_declarations(
    syntax: &syn::File,
    relative: &Path,
    declarations: &mut BTreeMap<String, Vec<ModuleDeclaration>>,
) {
    let mut collector = DeclarationCollector {
        declaring_file: path_text(relative),
        // The base `#[path]` resolves against: the containing file's
        // directory. Verified against
        // `apps/remi/src/server/catalog_authority.rs`, whose
        // `#[path = "catalog_authority/tests/test_support.rs"]` resolves below
        // `apps/remi/src/server/`, and against `apps/conary/build.rs`, whose
        // `#[path = "src/cli/mod.rs"]` resolves below `apps/conary/`.
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
    file_dir: PathBuf,
    declarations: &'a mut BTreeMap<String, Vec<ModuleDeclaration>>,
}

impl DeclarationCollector<'_> {
    fn run(&mut self, items: &[Item], module_dir: &Path, file_attributes: &[Attribute]) {
        let path_base = self.file_dir.clone();
        self.collect(items, module_dir, &path_base, file_attributes, 0);
    }

    fn collect(
        &mut self,
        items: &[Item],
        module_dir: &Path,
        path_base: &Path,
        inherited: &[Attribute],
        depth: usize,
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
                // attribute on it renames that directory, and a `#[path]`
                // inside it resolves against that same directory.
                let nested_dir = match &declared_path {
                    Some(path) => module_dir.join(path),
                    None => module_dir.join(&name),
                };
                self.collect(items, &nested_dir, &nested_dir, &effective, depth + 1);
                continue;
            }

            match declared_path {
                Some(path) => {
                    let target = normalize(&path_base.join(path));
                    self.record(
                        target,
                        Some(name),
                        DeclarationKind::Exact,
                        depth,
                        test_gated,
                        true,
                    );
                }
                None => {
                    // Rust loads `<module>/<name>.rs` or, failing that,
                    // `<module>/<name>/mod.rs`.
                    self.record(
                        module_dir.join(format!("{name}.rs")),
                        Some(name.clone()),
                        DeclarationKind::Flat,
                        depth,
                        test_gated,
                        true,
                    );
                    self.record(
                        module_dir.join(&name).join("mod.rs"),
                        Some(name),
                        DeclarationKind::ModuleRoot,
                        depth,
                        test_gated,
                        true,
                    );
                }
            }
        }
    }

    fn record(
        &mut self,
        target: PathBuf,
        name: Option<String>,
        kind: DeclarationKind,
        depth: usize,
        test_gated: bool,
        is_module: bool,
    ) {
        self.declarations
            .entry(path_text(&target))
            .or_default()
            .push(ModuleDeclaration {
                declaring_file: self.declaring_file.clone(),
                name,
                kind,
                depth,
                test_gated,
                is_module,
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
        // The included path shares `#[path]`'s base: the containing file's
        // directory.
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
                name: None,
                kind: DeclarationKind::Exact,
                depth: 0,
                test_gated: cfg::is_test_only(&effective),
                is_module: false,
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

/// The status a file starts from before any declaring site is read: an inner
/// `#![cfg(test)]` is intrinsic and hard, a crate root that is not a test
/// target is production, and everything else starts unknown.
pub(crate) fn intrinsic_gate(syntax: &syn::File, relative: &Path) -> ExemptionGate {
    if cfg::is_test_only(&syntax.attrs) {
        ExemptionGate::TestGated
    } else if non_test_crate_root(relative) {
        ExemptionGate::Ungated
    } else {
        ExemptionGate::Unknown
    }
}

/// Resolve every file's gate from the declaration graph until it stabilizes.
///
/// A determined answer is never revised, which is what makes the iteration
/// terminate and order-independent: an intrinsic gate is permanent, and a file
/// can only be determined from sites whose declaring files are already
/// determined (or whose site gate is a property of the declaration itself).
pub(crate) fn resolve_gates(
    intrinsic: &BTreeMap<String, ExemptionGate>,
    declarations: &BTreeMap<String, Vec<ModuleDeclaration>>,
) -> BTreeMap<String, ExemptionGate> {
    let mut gates = intrinsic.clone();
    // A file nothing declares is still resolvable — a cargo test target is
    // compiled as one without any declaring site — so both keys are considered.
    let mut targets = gates.keys().cloned().collect::<BTreeSet<_>>();
    targets.extend(declarations.keys().cloned());
    loop {
        let mut changed = false;
        for target in &targets {
            if resolved_gate(&gates, target) != ExemptionGate::Unknown {
                continue;
            }
            let sites = declarations
                .get(target)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let Some(gate) = gate_from_sites(target, sites, &gates) else {
                continue;
            };
            gates.insert(target.clone(), gate);
            changed = true;
        }
        if !changed {
            return gates;
        }
    }
}

/// The gate one declaring site compiles its target in, given the declaring
/// file's own gate. A site that is not test-gated passes the declaring file's
/// context through unchanged, which is how production reachability propagates.
fn site_context(
    site: &ModuleDeclaration,
    gates: &BTreeMap<String, ExemptionGate>,
) -> ExemptionGate {
    if site.test_gated {
        return ExemptionGate::TestGated;
    }
    resolved_gate(gates, &site.declaring_file)
}

/// `None` while the declaring sites establish neither answer.
fn gate_from_sites(
    target: &str,
    sites: &[ModuleDeclaration],
    gates: &BTreeMap<String, ExemptionGate>,
) -> Option<ExemptionGate> {
    let contexts = sites
        .iter()
        .map(|site| site_context(site, gates))
        .collect::<Vec<_>>();
    // A demonstrated non-test reachability always wins over test-target
    // membership: the same file can be a cargo test target and be imported by
    // the production library, and then it is ordinary production-capable code.
    if contexts.contains(&ExemptionGate::Ungated) {
        return Some(ExemptionGate::Ungated);
    }
    if !contexts.is_empty()
        && contexts
            .iter()
            .all(|context| *context == ExemptionGate::TestGated)
    {
        return Some(ExemptionGate::TestGated);
    }
    // Cargo test-target membership is a context, so it certifies a file only
    // when nothing else claims to compile it.
    if contexts.is_empty() && cargo_target_root(Path::new(target)) == Some(CargoTarget::Test) {
        return Some(ExemptionGate::TestGated);
    }
    None
}

/// Resolve the top-level `mod name;` children of one declaring file to the
/// files rustc would load, keeping only files in `scanned`. An unresolvable
/// declaration is dropped rather than guessed at.
///
/// Shared with `siblings`, so the file a parent row attributes and the file the
/// exemption classifier gates are always the same file.
pub(crate) fn resolve_child_modules(
    declaring_file: &str,
    declarations: &BTreeMap<String, Vec<ModuleDeclaration>>,
    scanned: &BTreeSet<String>,
) -> Vec<String> {
    let mut exact = BTreeSet::new();
    let mut flat = BTreeMap::new();
    let mut module_root = BTreeMap::new();
    for (target, sites) in declarations {
        for site in sites {
            if !site.is_module || site.depth != 0 || site.declaring_file != declaring_file {
                continue;
            }
            if !scanned.contains(target) {
                continue;
            }
            match (site.kind, &site.name) {
                (DeclarationKind::Exact, _) => {
                    exact.insert(target.clone());
                }
                (DeclarationKind::Flat, Some(name)) => {
                    flat.insert(name.clone(), target.clone());
                }
                (DeclarationKind::ModuleRoot, Some(name)) => {
                    module_root.insert(name.clone(), target.clone());
                }
                (DeclarationKind::Flat | DeclarationKind::ModuleRoot, None) => {}
            }
        }
    }
    // rustc prefers `<name>.rs` over `<name>/mod.rs` when both exist.
    let mut chosen = flat;
    for (name, target) in module_root {
        chosen.entry(name).or_insert(target);
    }
    exact.extend(chosen.into_values());
    exact.into_iter().collect()
}

/// How cargo compiles a file, when the file is a target root.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum CargoTarget {
    /// An integration test target (`<package>/tests/<name>.rs`). Cargo builds it
    /// only for `cargo test`, so the whole module tree below it is test-only
    /// unless some other declaration reaches it.
    Test,
    /// A bin, bench, or example target root: a crate root that owns its
    /// directory, and is not test-only.
    Other,
}

/// Cargo target roots relative to `apps/<package>/` or `crates/<package>/`.
/// Auto-discovery covers `<dir>/<name>.rs` and `<dir>/<name>/main.rs` below
/// `tests/`, `benches/`, `examples/`, and `src/bin/`. Discovery is by path
/// convention only: it does not read the Cargo manifest, so customized target
/// paths and `autotests` settings are not modelled.
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

/// Whether the file is a crate root cargo builds outside `cfg(test)`: a
/// package's `build.rs`, or `src/lib.rs` / `src/main.rs`. Target roots
/// (`tests/`, `benches/`, `examples/`, `src/bin/`) answer through
/// `cargo_target_root` instead.
fn non_test_crate_root(relative: &Path) -> bool {
    match cargo_target_root(relative) {
        Some(CargoTarget::Other) => return true,
        Some(CargoTarget::Test) => return false,
        None => {}
    }
    matches!(
        package_relative(relative).as_deref().and_then(Path::to_str),
        Some("build.rs" | "src/lib.rs" | "src/main.rs")
    )
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
/// `module_directory`, which is the same question for a bare parent directory.
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
/// `mod tests;` as `Foo/tests.rs`, while `Foo/mod.rs` declares it as
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

/// Resolve `.` and `..` components of a `#[path]` or `include!` value without
/// touching the filesystem, so `../../tests/common/update_ccs.rs` from
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

/// Every exempt-named file the cap check skipped, with the metrics the
/// `EXEMPT:` rows and the stale-entry rule are decided from.
#[derive(Default)]
pub(crate) struct ExemptionReport {
    files: Vec<ExemptFile>,
}

struct ExemptFile {
    relative: String,
    metrics: FileMetrics,
}

impl ExemptionReport {
    pub(crate) fn record(&mut self, relative: String, metrics: FileMetrics) {
        self.files.push(ExemptFile { relative, metrics });
    }

    /// The `EXEMPT:` rows and the `EXEMPT SUMMARY:` line, in scan order.
    pub(crate) fn report(&self, gates: &BTreeMap<String, ExemptionGate>) -> String {
        let mut test_gated = 0usize;
        let mut ungated = 0usize;
        let mut unknown = 0usize;
        let mut lines = String::new();
        for file in &self.files {
            let gate = resolved_gate(gates, &file.relative);
            match gate {
                ExemptionGate::TestGated => test_gated += 1,
                ExemptionGate::Ungated => ungated += 1,
                ExemptionGate::Unknown => unknown += 1,
            }
            lines.push_str(&format!(
                "EXEMPT: {}\ttotal={}\tproduction={}\tinline_test={}\tgate={}\n",
                file.relative,
                file.metrics.total_lines,
                file.metrics.production_lines,
                file.metrics.inline_test_lines,
                gate.label()
            ));
        }
        lines.push_str(&format!(
            "EXEMPT SUMMARY: test-gated={test_gated} ungated={ungated} unknown={unknown}\n"
        ));
        lines
    }
}
