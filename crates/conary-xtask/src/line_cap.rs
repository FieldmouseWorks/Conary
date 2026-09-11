// crates/conary-xtask/src/line_cap.rs

use proc_macro2::Span;
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{Attribute, Expr, ForeignItem, ImplItem, Item, Lit, Meta, TraitItem};

mod cfg;

const PRODUCTION_LINE_LIMIT: usize = 1_000;
const INLINE_TEST_LINE_LIMIT: usize = 300;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct LineSpan {
    start: usize,
    end: usize,
}

impl LineSpan {
    fn line_count(self) -> usize {
        self.end - self.start + 1
    }
}

#[derive(Debug, Eq, PartialEq)]
struct FileMetrics {
    total_lines: usize,
    production_lines: usize,
    inline_test_lines: usize,
}

#[derive(Debug)]
struct Options {
    root: PathBuf,
    allowlist: PathBuf,
    report: bool,
}

pub(crate) fn run(args: impl Iterator<Item = String>) -> Result<(), String> {
    let args = args.collect::<Vec<_>>();
    if args
        .iter()
        .any(|argument| argument == "-h" || argument == "--help")
    {
        println!(
            "Usage: cargo run -q -p conary-xtask -- line-cap --allowlist <path> [--root <path>] [--report]"
        );
        return Ok(());
    }
    let options = Options::parse(args.into_iter())?;
    let root = options.root.canonicalize().map_err(|error| {
        format!(
            "cannot resolve scan root {}: {error}",
            options.root.display()
        )
    })?;
    let allowlist = read_allowlist(&options.allowlist)?;
    let files = rust_source_files(&root)?;
    let mut used_allowlist_entries = BTreeSet::new();
    let mut errors = Vec::new();
    let mut declarations: BTreeMap<String, Vec<ModuleDeclaration>> = BTreeMap::new();
    let mut file_gates: BTreeMap<String, bool> = BTreeMap::new();
    let mut exempt_files: Vec<(String, FileMetrics)> = Vec::new();

    for path in files {
        let relative = path
            .strip_prefix(&root)
            .map_err(|error| format!("cannot relativize {}: {error}", path.display()))?;
        let relative_path = relative;
        let relative = path_text(relative_path);
        let source = match fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) => {
                errors.push(format!("cannot read {relative}: {error}"));
                continue;
            }
        };
        if let Err(error) = validate_path_comment(&source, relative_path) {
            errors.push(error);
        }
        let syntax = match syn::parse_file(&source) {
            Ok(syntax) => syntax,
            Err(error) => {
                errors.push(format!("failed to parse {relative}: {error}"));
                continue;
            }
        };
        let metrics = measure_source(&syntax, &source);
        collect_module_declarations(&syntax, relative_path, &mut declarations);
        collect_include_declarations(&syntax, relative_path, &mut declarations);
        file_gates.insert(
            relative.clone(),
            file_level_test_gate(&syntax, relative_path),
        );

        if excluded_test_file(relative_path) {
            exempt_files.push((relative, metrics));
            continue;
        }

        if options.report {
            println!(
                "{relative}\ttotal={}\tproduction={}\tinline_test={}",
                metrics.total_lines, metrics.production_lines, metrics.inline_test_lines
            );
        }

        let production_over = metrics.production_lines > PRODUCTION_LINE_LIMIT;
        let inline_test_over = metrics.inline_test_lines > INLINE_TEST_LINE_LIMIT;
        if !production_over && !inline_test_over {
            continue;
        }

        if let Some(issue) = allowlist.get(&relative) {
            println!(
                "ALLOWLISTED: {relative} production={} inline_test={} issue={issue}",
                metrics.production_lines, metrics.inline_test_lines
            );
            used_allowlist_entries.insert(relative);
            continue;
        }

        if production_over {
            errors.push(format!(
                "{relative} has {} non-test lines (limit: {PRODUCTION_LINE_LIMIT})",
                metrics.production_lines
            ));
        }
        if inline_test_over {
            errors.push(format!(
                "{relative} has {} inline test lines (limit: {INLINE_TEST_LINE_LIMIT})",
                metrics.inline_test_lines
            ));
        }
    }

    // Issue #997: an exempt-named file is reported and classified instead of
    // being silently dropped. Its exemption is established from the module
    // declaration graph (and the file's own inner attributes), never from the
    // file's text alone, because a test-named helper such as
    // `catalog_authority/tests/test_support.rs` looks like production code when
    // parsed standalone while its declaring site is `#[cfg(test)]`-gated.
    //
    // This increment is report-only: exempt files stay uncapped, so no
    // classification can add or remove an error.
    let resolved_gates = resolve_test_gates(&file_gates, &declarations);
    let mut test_gated_exemptions = 0usize;
    let mut ungated_exemptions = 0usize;
    for (relative, metrics) in &exempt_files {
        let gate = exemption_gate(resolved_gates.get(relative).copied().unwrap_or(false));
        match gate {
            ExemptionGate::TestGated => test_gated_exemptions += 1,
            ExemptionGate::Ungated => ungated_exemptions += 1,
        }
        if options.report {
            println!(
                "EXEMPT: {relative}\ttotal={}\tproduction={}\tinline_test={}\tgate={}",
                metrics.total_lines,
                metrics.production_lines,
                metrics.inline_test_lines,
                gate.label()
            );
        }
        // Issue #997: exempt-named files used to skip the allowlist bookkeeping
        // entirely, so a listed entry for one of them stayed unused and the
        // stale-entry sweep below rejected it: an over-cap exempt file could
        // never be allowlisted. Record a listed exempt file as used. The file
        // stays uncapped in this report-only increment, so no cap outcome
        // changes.
        if allowlist.contains_key(relative) {
            used_allowlist_entries.insert(relative.clone());
        }
    }
    if options.report {
        println!("EXEMPT SUMMARY: test-gated={test_gated_exemptions} ungated={ungated_exemptions}");
    }

    for (path, issue) in allowlist {
        if !used_allowlist_entries.contains(&path) {
            errors.push(format!("stale line-cap allowlist entry: {path} {issue}"));
        }
    }

    if errors.is_empty() {
        println!("Rust source line caps passed.");
        return Ok(());
    }

    for error in errors {
        eprintln!("ERROR: {error}");
    }
    Err("Rust source line caps failed".to_string())
}

impl Options {
    fn parse(mut args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut root = env::current_dir().map_err(|error| format!("cannot read cwd: {error}"))?;
        let mut allowlist = None;
        let mut report = false;

        while let Some(argument) = args.next() {
            match argument.as_str() {
                "--root" => {
                    root = PathBuf::from(
                        args.next()
                            .ok_or_else(|| "--root requires a path".to_string())?,
                    );
                }
                "--allowlist" => {
                    allowlist = Some(PathBuf::from(
                        args.next()
                            .ok_or_else(|| "--allowlist requires a path".to_string())?,
                    ));
                }
                "--report" => report = true,
                _ => return Err(format!("unknown line-cap argument: {argument}")),
            }
        }

        let allowlist = allowlist.ok_or_else(|| "--allowlist requires a path".to_string())?;
        Ok(Self {
            root,
            allowlist,
            report,
        })
    }
}

fn read_allowlist(path: &Path) -> Result<BTreeMap<String, String>, String> {
    let contents = fs::read_to_string(path)
        .map_err(|error| format!("line-cap allowlist not found: {}: {error}", path.display()))?;
    let mut entries = BTreeMap::new();

    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 2 || !valid_issue(fields[1]) {
            return Err(format!(
                "invalid allowlist entry (expected '<path> #<issue>'): {line}"
            ));
        }
        if entries
            .insert(fields[0].to_string(), fields[1].to_string())
            .is_some()
        {
            return Err(format!("duplicate allowlist entry: {}", fields[0]));
        }
    }

    Ok(entries)
}

fn valid_issue(value: &str) -> bool {
    value
        .strip_prefix('#')
        .and_then(|number| number.parse::<u64>().ok())
        .is_some_and(|number| number > 0)
}

fn rust_source_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    let mut found_source_root = false;
    for source_root in [root.join("apps"), root.join("crates")] {
        if !source_root.is_dir() {
            continue;
        }
        found_source_root = true;
        collect_rust_files(&source_root, &mut files)?;
    }
    if !found_source_root {
        return Err(format!(
            "no apps/ or crates/ source roots below {}",
            root.display()
        ));
    }
    files.sort();
    Ok(files)
}

fn collect_rust_files(directory: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
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

fn excluded_test_file(relative: &Path) -> bool {
    relative.file_name() == Some(OsStr::new("tests.rs"))
        || relative
            .components()
            .any(|component| component == Component::Normal(OsStr::new("tests")))
}

fn validate_path_comment(source: &str, relative: &Path) -> Result<(), String> {
    let first_line = source.lines().next().unwrap_or_default();
    let expected = path_text(relative);
    if first_line == format!("// {expected}") {
        Ok(())
    } else {
        Err(format!(
            "{expected} has mismatched path comment: expected `// {expected}`, found `{first_line}`"
        ))
    }
}

fn measure_source(syntax: &syn::File, source: &str) -> FileMetrics {
    let total_lines = source.lines().count();
    // Inner attributes such as `#![cfg(test)]` gate the whole file; syn keeps them
    // on `File::attrs`, which no descendant visit sees.
    let spans = if total_lines > 0 && cfg::is_test_only(&syntax.attrs) {
        vec![LineSpan {
            start: 1,
            end: total_lines,
        }]
    } else {
        // A production-capable file still narrows every child by its own cfg
        // attributes; children are classified under that conjunction.
        let mut visitor = TestSpanVisitor {
            inherited: cfg_attributes(&syntax.attrs),
            spans: Vec::new(),
        };
        visitor.visit_file(syntax);
        union_spans(visitor.spans)
    };
    let inline_test_lines = spans.iter().copied().map(LineSpan::line_count).sum();
    FileMetrics {
        total_lines,
        production_lines: total_lines.saturating_sub(inline_test_lines),
        inline_test_lines,
    }
}

#[derive(Default)]
struct TestSpanVisitor {
    // cfg/cfg_attr attributes of every enclosing production-capable node, in
    // order; a child exists only where this conjunction and its own attributes hold.
    inherited: Vec<Attribute>,
    spans: Vec<LineSpan>,
}

impl TestSpanVisitor {
    fn record(&mut self, attributes: &[Attribute], span: Span) -> bool {
        let mut effective = self.inherited.clone();
        effective.extend(attributes.iter().cloned());
        if !cfg::is_test_only(&effective) {
            return false;
        }
        let start = attributes
            .iter()
            .map(|attribute| attribute.span().start().line)
            .min()
            .unwrap_or_else(|| span.start().line);
        self.spans.push(LineSpan {
            start,
            end: span.end().line,
        });
        true
    }

    // Record a test-only node, or descend into a production-capable one with its
    // cfg constraints pushed onto the inherited stack for the duration.
    fn descend(&mut self, attributes: &[Attribute], span: Span, visit: impl FnOnce(&mut Self)) {
        if self.record(attributes, span) {
            return;
        }
        let depth = self.inherited.len();
        self.inherited.extend(cfg_attributes(attributes));
        visit(self);
        self.inherited.truncate(depth);
    }
}

fn cfg_attributes(attributes: &[Attribute]) -> Vec<Attribute> {
    attributes
        .iter()
        .filter(|attribute| {
            attribute.path().is_ident("cfg") || attribute.path().is_ident("cfg_attr")
        })
        .cloned()
        .collect()
}

// Each of these typed nodes owns outer attributes and an exact syntax span.
macro_rules! visit_attributed_nodes {
    ($($method:ident: $node:ident),* $(,)?) => {$ (
        fn $method(&mut self, node: &'ast syn::$node) {
            self.descend(&node.attrs, node.span(), |visitor| visit::$method(visitor, node));
        }
    )*};
}

impl<'ast> Visit<'ast> for TestSpanVisitor {
    visit_attributed_nodes! {
        visit_field: Field,
        visit_variant: Variant,
        visit_local: Local,
        visit_stmt_macro: StmtMacro,
        visit_arm: Arm,
        visit_field_value: FieldValue,
        visit_expr_array: ExprArray,
        visit_expr_assign: ExprAssign,
        visit_expr_async: ExprAsync,
        visit_expr_await: ExprAwait,
        visit_expr_binary: ExprBinary,
        visit_expr_block: ExprBlock,
        visit_expr_break: ExprBreak,
        visit_expr_call: ExprCall,
        visit_expr_cast: ExprCast,
        visit_expr_closure: ExprClosure,
        visit_expr_const: ExprConst,
        visit_expr_continue: ExprContinue,
        visit_expr_field: ExprField,
        visit_expr_for_loop: ExprForLoop,
        visit_expr_group: ExprGroup,
        visit_expr_if: ExprIf,
        visit_expr_index: ExprIndex,
        visit_expr_infer: ExprInfer,
        visit_expr_let: ExprLet,
        visit_expr_lit: ExprLit,
        visit_expr_loop: ExprLoop,
        visit_expr_macro: ExprMacro,
        visit_expr_match: ExprMatch,
        visit_expr_method_call: ExprMethodCall,
        visit_expr_paren: ExprParen,
        visit_expr_path: ExprPath,
        visit_expr_range: ExprRange,
        visit_expr_raw_addr: ExprRawAddr,
        visit_expr_reference: ExprReference,
        visit_expr_repeat: ExprRepeat,
        visit_expr_return: ExprReturn,
        visit_expr_struct: ExprStruct,
        visit_expr_try: ExprTry,
        visit_expr_try_block: ExprTryBlock,
        visit_expr_tuple: ExprTuple,
        visit_expr_unary: ExprUnary,
        visit_expr_unsafe: ExprUnsafe,
        visit_expr_while: ExprWhile,
        visit_expr_yield: ExprYield,
        visit_pat_ident: PatIdent,
        visit_pat_or: PatOr,
        visit_pat_paren: PatParen,
        visit_pat_reference: PatReference,
        visit_pat_rest: PatRest,
        visit_pat_slice: PatSlice,
        visit_pat_struct: PatStruct,
        visit_pat_tuple: PatTuple,
        visit_pat_tuple_struct: PatTupleStruct,
        visit_pat_type: PatType,
        visit_pat_wild: PatWild,
        visit_field_pat: FieldPat,
        visit_receiver: Receiver,
        visit_variadic: Variadic,
        visit_type_param: TypeParam,
        visit_const_param: ConstParam,
        visit_lifetime_param: LifetimeParam,
    }

    fn visit_item(&mut self, item: &'ast Item) {
        self.descend(item_attributes(item), item.span(), |visitor| {
            visit::visit_item(visitor, item)
        });
    }

    fn visit_impl_item(&mut self, item: &'ast ImplItem) {
        self.descend(impl_item_attributes(item), item.span(), |visitor| {
            visit::visit_impl_item(visitor, item)
        });
    }

    fn visit_trait_item(&mut self, item: &'ast TraitItem) {
        self.descend(trait_item_attributes(item), item.span(), |visitor| {
            visit::visit_trait_item(visitor, item)
        });
    }

    fn visit_foreign_item(&mut self, item: &'ast ForeignItem) {
        self.descend(foreign_item_attributes(item), item.span(), |visitor| {
            visit::visit_foreign_item(visitor, item)
        });
    }
}

fn item_attributes(item: &Item) -> &[Attribute] {
    match item {
        Item::Const(item) => &item.attrs,
        Item::Enum(item) => &item.attrs,
        Item::ExternCrate(item) => &item.attrs,
        Item::Fn(item) => &item.attrs,
        Item::ForeignMod(item) => &item.attrs,
        Item::Impl(item) => &item.attrs,
        Item::Macro(item) => &item.attrs,
        Item::Mod(item) => &item.attrs,
        Item::Static(item) => &item.attrs,
        Item::Struct(item) => &item.attrs,
        Item::Trait(item) => &item.attrs,
        Item::TraitAlias(item) => &item.attrs,
        Item::Type(item) => &item.attrs,
        Item::Union(item) => &item.attrs,
        Item::Use(item) => &item.attrs,
        Item::Verbatim(_) => &[],
        _ => &[],
    }
}

fn impl_item_attributes(item: &ImplItem) -> &[Attribute] {
    match item {
        ImplItem::Const(item) => &item.attrs,
        ImplItem::Fn(item) => &item.attrs,
        ImplItem::Type(item) => &item.attrs,
        ImplItem::Macro(item) => &item.attrs,
        ImplItem::Verbatim(_) => &[],
        _ => &[],
    }
}

fn trait_item_attributes(item: &TraitItem) -> &[Attribute] {
    match item {
        TraitItem::Const(item) => &item.attrs,
        TraitItem::Fn(item) => &item.attrs,
        TraitItem::Type(item) => &item.attrs,
        TraitItem::Macro(item) => &item.attrs,
        TraitItem::Verbatim(_) => &[],
        _ => &[],
    }
}

fn foreign_item_attributes(item: &ForeignItem) -> &[Attribute] {
    match item {
        ForeignItem::Fn(item) => &item.attrs,
        ForeignItem::Static(item) => &item.attrs,
        ForeignItem::Type(item) => &item.attrs,
        ForeignItem::Macro(item) => &item.attrs,
        ForeignItem::Verbatim(_) => &[],
        _ => &[],
    }
}

fn union_spans(mut spans: Vec<LineSpan>) -> Vec<LineSpan> {
    spans.sort_by_key(|span| (span.start, span.end));
    let mut union: Vec<LineSpan> = Vec::new();
    for span in spans {
        if let Some(previous) = union.last_mut()
            && span.start <= previous.end.saturating_add(1)
        {
            previous.end = previous.end.max(span.end);
        } else {
            union.push(span);
        }
    }
    union
}

fn path_text(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// How an exempt-named file earns its exemption from the line caps.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum ExemptionGate {
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
    fn label(self) -> &'static str {
        match self {
            Self::TestGated => "test-gated",
            Self::Ungated => "ungated",
        }
    }
}

fn exemption_gate(test_gated: bool) -> ExemptionGate {
    if test_gated {
        ExemptionGate::TestGated
    } else {
        ExemptionGate::Ungated
    }
}

/// A `mod x;` declaration that makes one file a module of another.
#[derive(Debug)]
struct ModuleDeclaration {
    declaring_file: String,
    /// The declaration's own gate: its `cfg`/`cfg_attr` attributes conjoined
    /// with those of every enclosing inline module.
    test_gated: bool,
}

/// The file's own gate, before any declaring site is considered.
fn file_level_test_gate(syntax: &syn::File, relative: &Path) -> bool {
    cfg::is_test_only(&syntax.attrs) || cargo_target_root(relative) == Some(CargoTarget::Test)
}

/// Index every external module declaration by the repo-relative path Rust would
/// load for it, so exempt-named files can be classified by their declaring
/// sites rather than by their own text.
fn collect_module_declarations(
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
    let module_dir = module_directory(relative);
    collector.run(&syntax.items, &module_dir, &syntax.attrs);
}

struct DeclarationCollector<'a> {
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
fn collect_include_declarations(
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

struct IncludeVisitor<'a> {
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
fn resolve_test_gates(
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
enum CargoTarget {
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
fn cargo_target_root(relative: &Path) -> Option<CargoTarget> {
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

fn package_relative(relative: &Path) -> Option<PathBuf> {
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

fn normal_text(component: Component<'_>) -> Option<String> {
    match component {
        Component::Normal(text) => Some(text.to_string_lossy().into_owned()),
        _ => None,
    }
}

/// The directory Rust searches for `mod x;` declared by this file.
fn module_directory(relative: &Path) -> PathBuf {
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
fn owns_its_directory(relative: &Path) -> bool {
    let name = relative.file_name().and_then(OsStr::to_str);
    matches!(name, Some("mod.rs" | "lib.rs" | "main.rs" | "build.rs"))
        || cargo_target_root(relative).is_some()
}

fn path_attribute(attributes: &[Attribute]) -> Option<String> {
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
fn normalize(path: &Path) -> PathBuf {
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

#[cfg(test)]
#[path = "line_cap/tests.rs"]
mod tests;
