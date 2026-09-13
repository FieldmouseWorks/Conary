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
//! Only a resolved test-only context earns a filename exemption. Ungated and
//! unknown files retain the normal caps and can use an issue-owned exception.
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
//! Cargo target evidence comes from versioned `cargo metadata`, including
//! custom target paths and auto-discovery settings. Literal module/include
//! paths and conditional path alternatives participate in the source graph;
//! unresolved files stay unknown. Filenames alone never establish test-only
//! authority. This is a syntax gate, not macro expansion or type checking.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

use syn::ext::IdentExt;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{Attribute, Expr, Item, Lit};

use super::attributes::{visit_attributed_nodes, visit_nodes};
use super::cfg;

mod builtin_attributes;
mod graph;
mod macros;
use super::paths::module_paths;
use super::targets::TargetRoots;
use super::{
    FileMetrics, cfg_attributes, foreign_item_attributes, impl_item_attributes, item_attributes,
    path_text, trait_item_attributes,
};
pub(crate) use graph::{SourceGraph, collect_source_graph};

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

/// A conventional outlined module has exactly two candidate source paths.
/// Explicit paths and includes do not acquire a conventional fallback.
fn alternate_module_path(target: &Path, kind: DeclarationKind) -> Option<PathBuf> {
    match kind {
        DeclarationKind::Exact => None,
        DeclarationKind::Flat => Some(target.with_extension("").join("mod.rs")),
        DeclarationKind::ModuleRoot => Some(target.parent()?.with_extension("rs")),
    }
}

/// One declaring site that makes a file part of another file's compilation.
#[derive(Debug, Clone)]
pub(crate) struct ModuleDeclaration<K = String> {
    declaring_file: K,
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
    module_dir: &Path,
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

    collector.run(&syntax.items, module_dir, &syntax.attrs);
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
                // Item declarations inside function/const/expression blocks
                // remain real imports, but are not the parent's own siblings.
                NestedModuleVisitor {
                    collector: self,
                    module_dir,
                    path_base,
                    inherited: inherited.to_vec(),
                    depth: depth + 1,
                }
                .visit_item(item);
                continue;
            };
            let mut effective = inherited.to_vec();
            effective.extend(cfg_attributes(&item_module.attrs));
            let name = item_module.ident.unraw().to_string();
            for variant in module_paths(&item_module.attrs) {
                let mut branch = effective.clone();
                branch.extend(variant.conditions);
                if !cfg::can_compile(&branch) {
                    continue;
                }
                let test_gated = cfg::is_test_only(&branch);
                if let Some((_, items)) = &item_module.content {
                    let nested_dir = match variant.path {
                        Some(path) => path_base.join(path),
                        None => module_dir.join(&name),
                    };
                    self.collect(items, &nested_dir, &nested_dir, &branch, depth + 1);
                } else if let Some(path) = variant.path {
                    self.record(
                        normalize(&path_base.join(path)),
                        Some(name.clone()),
                        DeclarationKind::Exact,
                        depth,
                        test_gated,
                        true,
                    );
                } else {
                    // Both candidates are considered; simultaneous files are
                    // ambiguous and cannot contribute to sibling attribution.
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
                        Some(name.clone()),
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

/// Follow module items inside attributed non-module syntax using the same
/// conditions as includes. The collector still owns path variants and module
/// directories; this visitor only discovers nested item declarations.
struct NestedModuleVisitor<'a, 'b> {
    collector: &'a mut DeclarationCollector<'b>,
    module_dir: &'a Path,
    path_base: &'a Path,
    inherited: Vec<Attribute>,
    depth: usize,
}

impl NestedModuleVisitor<'_, '_> {
    fn descend(
        &mut self,
        attributes: &[Attribute],
        _: proc_macro2::Span,
        visit: impl FnOnce(&mut Self),
    ) {
        let depth = self.inherited.len();
        self.inherited.extend_from_slice(attributes);
        if cfg::can_compile(&self.inherited) {
            visit(self);
        }
        self.inherited.truncate(depth);
    }
}

impl<'ast> Visit<'ast> for NestedModuleVisitor<'_, '_> {
    visit_attributed_nodes!();

    fn visit_item(&mut self, item: &'ast Item) {
        if matches!(item, Item::Mod(_)) {
            self.collector.collect(
                std::slice::from_ref(item),
                self.module_dir,
                self.path_base,
                &self.inherited,
                self.depth,
            );
        } else {
            self.descend(item_attributes(item), item.span(), |visitor| {
                visit::visit_item(visitor, item)
            });
        }
    }

    fn visit_impl_item(&mut self, item: &'ast syn::ImplItem) {
        self.descend(impl_item_attributes(item), item.span(), |visitor| {
            visit::visit_impl_item(visitor, item)
        });
    }

    fn visit_trait_item(&mut self, item: &'ast syn::TraitItem) {
        self.descend(trait_item_attributes(item), item.span(), |visitor| {
            visit::visit_trait_item(visitor, item)
        });
    }

    fn visit_foreign_item(&mut self, item: &'ast syn::ForeignItem) {
        self.descend(foreign_item_attributes(item), item.span(), |visitor| {
            visit::visit_foreign_item(visitor, item)
        });
    }
}

/// Index textual `include!("x.rs")` sites as declaring sites too. An included
/// file is not a module, but its tokens are compiled in the including module, so
/// a `#[cfg(test)]` include site is exactly as strong as a gated declaration.
/// This is how `crates/conary-core/src/repository/sync/tests.rs` and its
/// `tests/native.rs` chain are compiled: `sync.rs` ends with
/// `#[cfg(test)] include!("sync/tests.rs");`.
#[cfg(test)]
pub(crate) fn collect_include_declarations(
    syntax: &syn::File,
    relative: &Path,
    declarations: &mut BTreeMap<String, Vec<ModuleDeclaration>>,
) -> Result<(), String> {
    collect_includes_with_authority(syntax, relative, declarations).map(|_| ())
}

fn collect_includes_with_authority(
    syntax: &syn::File,
    relative: &Path,
    declarations: &mut BTreeMap<String, Vec<ModuleDeclaration>>,
) -> Result<bool, String> {
    let mut visitor = IncludeVisitor {
        declaring_file: path_text(relative),
        // The included path shares `#[path]`'s base: the containing file's
        // directory.
        file_dir: relative
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .to_path_buf(),
        inherited: syntax.attrs.clone(),
        unresolved: None,
        production_macro: false,
        declarations,
    };
    if macros::attributes_require_expansion(&syntax.attrs, &syntax.attrs) {
        visitor.note(SourceAuthorityFailure::Expansion);
    }
    visitor.visit_file(syntax);
    if let Some(failure) = visitor.unresolved {
        Err(format!(
            "cannot resolve Rust source authority in {}: {}",
            relative.display(),
            failure.reason()
        ))
    } else {
        Ok(visitor.production_macro)
    }
}

enum SourceAuthorityFailure {
    Path,
    Alias,
    Shadow,
    ExternalImport,
    Expansion,
}

impl SourceAuthorityFailure {
    fn reason(&self) -> &'static str {
        match self {
            Self::Path => {
                "expected a builtin include! (unqualified, std::, or core::) with a string literal or literal concat! path"
            }
            Self::Alias => "aliased include! imports require macro name resolution",
            Self::Shadow => "local bindings shadow builtin include!/concat! macro authority",
            Self::ExternalImport => {
                "macro_use extern crate imports require macro name resolution and expansion"
            }
            Self::Expansion => "macro expansion may introduce source declarations",
        }
    }
}

fn include_import_failure(
    tree: &syn::UseTree,
    prefix: &[String],
) -> Option<SourceAuthorityFailure> {
    match tree {
        syn::UseTree::Path(path) => {
            let mut nested = prefix.to_vec();
            nested.push(path.ident.unraw().to_string());
            include_import_failure(&path.tree, &nested)
        }
        syn::UseTree::Group(group) => group
            .items
            .iter()
            .find_map(|tree| include_import_failure(tree, prefix)),
        syn::UseTree::Rename(rename) => {
            if rename.ident.unraw() == "include"
                && rename.rename.unraw() != "include"
                && rename.rename.unraw() != "_"
            {
                Some(SourceAuthorityFailure::Alias)
            } else {
                builtin_import_failure(
                    prefix,
                    &rename.ident.unraw().to_string(),
                    &rename.rename.unraw().to_string(),
                )
            }
        }
        syn::UseTree::Name(name) => builtin_import_failure(
            prefix,
            &name.ident.unraw().to_string(),
            &name.ident.unraw().to_string(),
        ),
        syn::UseTree::Glob(_) => Some(SourceAuthorityFailure::Shadow),
    }
}

fn builtin_import_failure(
    prefix: &[String],
    original: &str,
    binding: &str,
) -> Option<SourceAuthorityFailure> {
    if binding == "std" || binding == "core" {
        return Some(SourceAuthorityFailure::Shadow);
    }
    if binding != "include" && binding != "concat" {
        return None;
    }
    let builtin_namespace = prefix.is_empty()
        || matches!(prefix, [namespace] if namespace == "std" || namespace == "core");
    (!builtin_namespace || original != binding).then_some(SourceAuthorityFailure::Shadow)
}

pub(crate) struct IncludeVisitor<'a> {
    declaring_file: String,
    file_dir: PathBuf,
    inherited: Vec<Attribute>,
    unresolved: Option<SourceAuthorityFailure>,
    production_macro: bool,
    declarations: &'a mut BTreeMap<String, Vec<ModuleDeclaration>>,
}

impl IncludeVisitor<'_> {
    fn note(&mut self, failure: SourceAuthorityFailure) {
        if cfg::can_compile_without_test(&self.inherited) {
            self.unresolved.get_or_insert(failure);
        }
    }

    fn descend(
        &mut self,
        attributes: &[Attribute],
        _: proc_macro2::Span,
        visit: impl FnOnce(&mut Self),
    ) {
        let depth = self.inherited.len();
        self.inherited.extend_from_slice(attributes);
        if cfg::can_compile(&self.inherited) {
            if macros::attributes_require_expansion(attributes, &self.inherited) {
                self.note(SourceAuthorityFailure::Expansion);
            }
            visit(self);
        }
        self.inherited.truncate(depth);
    }

    fn record(&mut self, mac: &syn::Macro) {
        // Even std/core can be rebound through the compiler's extern prelude.
        // Literal paths remain useful declaration evidence, but their spelling
        // cannot certify that expansion adds no other production load sites.
        self.production_macro |= cfg::can_compile_without_test(&self.inherited);
        if !cfg::can_compile(&self.inherited) {
            return;
        }
        if !mac
            .path
            .segments
            .last()
            .is_some_and(|segment| segment.ident.unraw() == "include")
        {
            self.note(SourceAuthorityFailure::Expansion);
            return;
        }
        if !builtin_macro_path(&mac.path, "include") {
            self.note(SourceAuthorityFailure::Path);
            return;
        }
        use syn::parse::Parser;
        let parser = syn::punctuated::Punctuated::<Expr, syn::Token![,]>::parse_terminated;
        let Some(path) = parser
            .parse2(mac.tokens.clone())
            .ok()
            .filter(|arguments| arguments.len() == 1)
            .and_then(|mut arguments| arguments.pop())
            .and_then(|argument| include_path(argument.into_value()))
        else {
            // An opaque include may reach any scanned file, so contextual
            // exemptions cannot be certified from this incomplete graph.
            self.note(SourceAuthorityFailure::Path);
            return;
        };
        let target = normalize(&self.file_dir.join(path));
        self.declarations
            .entry(path_text(&target))
            .or_default()
            .push(ModuleDeclaration {
                declaring_file: self.declaring_file.clone(),
                name: None,
                kind: DeclarationKind::Exact,
                depth: 0,
                test_gated: cfg::is_test_only(&self.inherited),
                is_module: false,
            });
    }
}

/// Rust exports these builtins both unqualified and through std/core. Other
/// namespace-qualified include names may be custom macros; fail closed rather
/// than granting authority from their spelling or guessing their expansion.
fn builtin_macro_path(path: &syn::Path, name: &str) -> bool {
    if path
        .segments
        .iter()
        .any(|segment| !matches!(segment.arguments, syn::PathArguments::None))
    {
        return false;
    }
    let mut segments = path.segments.iter();
    match (segments.next(), segments.next(), segments.next()) {
        (Some(first), None, None) => first.ident.unraw() == name && path.leading_colon.is_none(),
        (Some(namespace), Some(last), None) => {
            (namespace.ident.unraw() == "std" || namespace.ident.unraw() == "core")
                && last.ident.unraw() == name
        }
        _ => false,
    }
}

fn include_path(expression: Expr) -> Option<String> {
    match expression {
        Expr::Lit(literal) => match literal.lit {
            Lit::Str(path) => Some(path.value()),
            _ => None,
        },
        Expr::Paren(parenthesized) => include_path(*parenthesized.expr),
        Expr::Macro(expression) if builtin_macro_path(&expression.mac.path, "concat") => {
            use syn::parse::Parser;
            let parser = syn::punctuated::Punctuated::<Expr, syn::Token![,]>::parse_terminated;
            parser
                .parse2(expression.mac.tokens)
                .ok()?
                .into_iter()
                .map(include_path)
                .collect::<Option<String>>()
        }
        _ => None,
    }
}

impl<'ast> Visit<'ast> for IncludeVisitor<'_> {
    visit_attributed_nodes!();

    fn visit_attribute(&mut self, node: &'ast Attribute) {
        // Inert compiler metadata accepts literal-valued macro expansion, not
        // source declarations. Do not treat its value as a source-load site.
        if builtin_attributes::parse(&node.meta).is_none() {
            visit::visit_attribute(self, node);
        }
    }

    fn visit_item(&mut self, item: &'ast Item) {
        self.descend(item_attributes(item), item.span(), |visitor| {
            visit::visit_item(visitor, item)
        });
    }

    fn visit_impl_item(&mut self, item: &'ast syn::ImplItem) {
        self.descend(impl_item_attributes(item), item.span(), |visitor| {
            visit::visit_impl_item(visitor, item)
        });
    }

    fn visit_trait_item(&mut self, item: &'ast syn::TraitItem) {
        self.descend(trait_item_attributes(item), item.span(), |visitor| {
            visit::visit_trait_item(visitor, item)
        });
    }

    fn visit_foreign_item(&mut self, item: &'ast syn::ForeignItem) {
        self.descend(foreign_item_attributes(item), item.span(), |visitor| {
            visit::visit_foreign_item(visitor, item)
        });
    }

    fn visit_item_macro(&mut self, node: &'ast syn::ItemMacro) {
        if super::attributes::is_ident(&node.mac.path, "macro_rules") {
            if node
                .ident
                .as_ref()
                .is_some_and(|ident| ident.unraw() == "include" || ident.unraw() == "concat")
            {
                self.note(SourceAuthorityFailure::Shadow);
            }
            // A definition does not expand until called. Every opaque call
            // requires compiler authority, regardless of its token spelling.
            return;
        }
        visit::visit_item_macro(self, node);
    }

    fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
        if let Some(failure) = include_import_failure(&node.tree, &[]) {
            self.note(failure);
        }
    }

    fn visit_item_extern_crate(&mut self, node: &'ast syn::ItemExternCrate) {
        if node
            .rename
            .as_ref()
            .is_some_and(|(_, name)| name.unraw() == "std" || name.unraw() == "core")
        {
            self.note(SourceAuthorityFailure::Shadow);
        }
        if macros::reachable_external_import(&node.attrs, &self.inherited) {
            self.note(SourceAuthorityFailure::ExternalImport);
        }
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        self.record(node);
    }
}

/// Resolve every load context's gate from the declaration graph until it stabilizes.
///
/// A determined answer is never revised, which is what makes the iteration
/// terminate and order-independent: an intrinsic gate is permanent, and a file
/// can only be determined from sites whose declaring files are already
/// determined (or whose site gate is a property of the declaration itself).
pub(crate) fn resolve_gates<K: Ord + Clone>(
    intrinsic: &BTreeMap<K, ExemptionGate>,
    declarations: &BTreeMap<K, Vec<ModuleDeclaration<K>>>,
    target_roots: &BTreeMap<K, CargoTarget>,
) -> BTreeMap<K, ExemptionGate> {
    let mut gates = intrinsic.clone();
    // A file nothing declares is still resolvable — a cargo test target is
    // compiled as one without any declaring site — so both keys are considered.
    let mut targets = gates.keys().cloned().collect::<BTreeSet<_>>();
    targets.extend(declarations.keys().cloned());
    loop {
        let mut changed = false;
        for target in &targets {
            if gates.get(target).copied().unwrap_or(ExemptionGate::Unknown)
                != ExemptionGate::Unknown
            {
                continue;
            }
            let sites = declarations
                .get(target)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let Some(gate) = gate_from_sites(target, sites, &gates, target_roots) else {
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
fn site_context<K: Ord>(
    site: &ModuleDeclaration<K>,
    gates: &BTreeMap<K, ExemptionGate>,
) -> ExemptionGate {
    if site.test_gated {
        return ExemptionGate::TestGated;
    }
    gates
        .get(&site.declaring_file)
        .copied()
        .unwrap_or(ExemptionGate::Unknown)
}

/// `None` while the declaring sites establish neither answer.
fn gate_from_sites<K: Ord>(
    target: &K,
    sites: &[ModuleDeclaration<K>],
    gates: &BTreeMap<K, ExemptionGate>,
    target_roots: &BTreeMap<K, CargoTarget>,
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
    if contexts.is_empty() && target_roots.get(target) == Some(&CargoTarget::Test) {
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
    let mut chosen = BTreeSet::new();
    for (target, sites) in declarations {
        for site in sites {
            if !site.is_module || site.depth != 0 || site.declaring_file != declaring_file {
                continue;
            }
            if !scanned.contains(target) {
                continue;
            }
            if site.kind != DeclarationKind::Exact && site.name.is_none() {
                continue;
            }
            // Rust rejects two existing conventional candidates. The same
            // pairing owns missing-load uncertainty in graph construction.
            if !alternate_module_path(Path::new(target), site.kind)
                .is_some_and(|alternate| scanned.contains(&path_text(&alternate)))
            {
                chosen.insert(target.clone());
            }
        }
    }
    chosen.into_iter().collect()
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

/// Resolve `.` and `..` components of a `#[path]` or `include!` value without
/// touching the filesystem, so `../../tests/common/update_ccs.rs` from
/// `apps/conary/src/commands/test_helpers.rs` becomes
/// `apps/conary/tests/common/update_ccs.rs`.
pub(crate) fn normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir
                if matches!(
                    normalized.components().next_back(),
                    Some(Component::Normal(_))
                ) =>
            {
                normalized.pop();
            }
            // A relative path can escape the scan root. Preserve that identity
            // instead of aliasing its suffix to a scanned repository source.
            Component::ParentDir if !normalized.has_root() => normalized.push(".."),
            Component::ParentDir => {}
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

/// Every file with a test filename, including files that retain normal caps.
/// The report exposes both the measured content and its resolved context.
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

    /// The `TEST FILE:` rows and the `TEST FILE SUMMARY:` line, in scan order.
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
                "TEST FILE: {}\ttotal={}\tproduction={}\tinline_test={}\tgate={}\n",
                file.relative,
                file.metrics.total_lines,
                file.metrics.production_lines,
                file.metrics.inline_test_lines,
                gate.label()
            ));
        }
        lines.push_str(&format!(
            "TEST FILE SUMMARY: test-gated={test_gated} ungated={ungated} unknown={unknown}\n"
        ));
        lines
    }
}
