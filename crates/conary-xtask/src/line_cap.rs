// crates/conary-xtask/src/line_cap.rs

use proc_macro2::Span;
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{Attribute, Expr, ExprLit, ForeignItem, ImplItem, Item, ItemMod, Lit, Meta, TraitItem};

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

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct FileMetrics {
    total_lines: usize,
    production_lines: usize,
    inline_test_lines: usize,
}

/// Lines a file moved out of itself into `mod name;` child modules.
///
/// `reduction` is deliberately reported as `sibling_lines`: the mass that would
/// be measured on the parent again if the same content were still declared
/// inline. This is a static attribution of where the lines are now, not a
/// git-verified before/after, so it never claims a historical delta.
#[derive(Debug, Default, Clone, Copy, Eq, PartialEq)]
struct SiblingAttribution {
    siblings: usize,
    sibling_lines: usize,
}

/// Metrics for scanned files, filled on demand so a parent can measure a child
/// before the scan reaches it. `analyze_source` is a pure function of the file
/// bytes, so a child measured early and in its own turn always agree.
#[derive(Default)]
struct MeasuredFiles {
    metrics: BTreeMap<PathBuf, Option<FileMetrics>>,
}

impl MeasuredFiles {
    fn insert(&mut self, path: &Path, metrics: FileMetrics) {
        self.metrics.insert(path.to_path_buf(), Some(metrics));
    }

    fn measure(&mut self, path: &Path) -> Option<FileMetrics> {
        if let Some(metrics) = self.metrics.get(path) {
            return *metrics;
        }
        let metrics = fs::read_to_string(path)
            .ok()
            .and_then(|source| analyze_source(&source).ok());
        self.metrics.insert(path.to_path_buf(), metrics);
        metrics
    }
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
    let scan = rust_source_files(&root)?;
    if options.report {
        println!("SOURCE ROOTS: {}", source_roots_text(&scan.coverage));
    }
    let scanned = scan.files.iter().cloned().collect::<BTreeSet<_>>();
    let mut measured = MeasuredFiles::default();
    let mut used_allowlist_entries = BTreeSet::new();
    let mut errors = Vec::new();

    for path in &scan.files {
        let relative = path
            .strip_prefix(&root)
            .map_err(|error| format!("cannot relativize {}: {error}", path.display()))?;
        let relative_path = relative;
        let relative = path_text(relative_path);
        let source = match fs::read_to_string(path) {
            Ok(source) => source,
            Err(error) => {
                errors.push(format!("cannot read {relative}: {error}"));
                continue;
            }
        };
        if let Err(error) = validate_path_comment(&source, relative_path) {
            errors.push(error);
        }
        let metrics = match analyze_source(&source) {
            Ok(metrics) => metrics,
            Err(error) => {
                errors.push(format!("failed to parse {relative}: {error}"));
                continue;
            }
        };
        measured.insert(path, metrics);

        if excluded_test_file(relative_path) {
            if options.report {
                // Exempt-named files stay out of the cap check and out of the
                // ordinary row stream; this distinct line only makes the mass a
                // parent reduced itself by visible.
                println!(
                    "EXTRACTED: {relative}\ttotal={}\tproduction={}\tinline_test={}",
                    metrics.total_lines, metrics.production_lines, metrics.inline_test_lines
                );
            }
            continue;
        }

        if options.report {
            let children = resolve_child_modules(path, &source, &scanned);
            let attribution = sibling_attribution(&children, &mut measured);
            println!("{}", report_row(&relative, metrics, attribution));
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

/// How a declared top-level Rust source root participates in the cap gate.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum RootPolicy {
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
struct SourceRoot {
    name: &'static str,
    policy: RootPolicy,
}

/// Every top-level directory that holds Rust built into the product, with the
/// policy the cap gate applies to it. A top-level directory carrying `.rs`
/// files that is absent here fails the gate until its policy is recorded, so a
/// new source root cannot escape measurement silently.
const SOURCE_ROOTS: &[SourceRoot] = &[
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
const IGNORED_DIRECTORY_NAMES: &[&str] = &["target", "node_modules", ".git", ".worktrees"];

/// One declared root's measured contribution to the coverage statement.
#[derive(Debug, Eq, PartialEq)]
struct RootCoverage {
    name: &'static str,
    policy: RootPolicy,
    files: usize,
}

/// How many files each declared root contributed, plus the files the gate
/// measures. Vendor-excluded roots are counted but never measured.
#[derive(Debug, Eq, PartialEq)]
struct SourceScan {
    files: Vec<PathBuf>,
    coverage: Vec<RootCoverage>,
}

fn declared_source_root(name: &str) -> Option<&'static SourceRoot> {
    SOURCE_ROOTS
        .iter()
        .find(|source_root| source_root.name == name)
}

fn skipped_directory(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    name.starts_with('.') || IGNORED_DIRECTORY_NAMES.contains(&name)
}

fn rust_source_files(root: &Path) -> Result<SourceScan, String> {
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

fn scan_source_roots(root: &Path) -> Result<SourceScan, String> {
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
fn source_roots_text(coverage: &[RootCoverage]) -> String {
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
fn undeclared_rust_roots(root: &Path) -> Result<Vec<String>, String> {
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
fn contains_rust_file(directory: &Path) -> Result<bool, String> {
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

/// One `--report` row. The first four fields keep their existing text and
/// order; the sibling fields are appended only when the file declares
/// out-of-line child modules that resolve to scanned files, so every row that
/// had no sibling keeps its exact previous shape.
fn report_row(relative: &str, metrics: FileMetrics, attribution: SiblingAttribution) -> String {
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
fn sibling_attribution(children: &[PathBuf], measured: &mut MeasuredFiles) -> SiblingAttribution {
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
fn resolve_child_modules(
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
fn module_directory(containing: &Path) -> Option<PathBuf> {
    let directory = containing.parent()?;
    if containing.file_name() == Some(OsStr::new("mod.rs")) {
        return Some(directory.to_path_buf());
    }
    Some(directory.join(containing.file_stem()?))
}

/// The literal `#[path = "..."]` value on a `mod` declaration, when present.
fn declared_module_path(declaration: &ItemMod) -> Option<String> {
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
fn normalize_path(path: PathBuf) -> PathBuf {
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

fn analyze_source(source: &str) -> syn::Result<FileMetrics> {
    let syntax = syn::parse_file(source)?;
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
        visitor.visit_file(&syntax);
        union_spans(visitor.spans)
    };
    let inline_test_lines = spans.iter().copied().map(LineSpan::line_count).sum();
    Ok(FileMetrics {
        total_lines,
        production_lines: total_lines.saturating_sub(inline_test_lines),
        inline_test_lines,
    })
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

#[cfg(test)]
#[path = "line_cap/tests.rs"]
mod tests;
