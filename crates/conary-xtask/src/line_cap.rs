// crates/conary-xtask/src/line_cap.rs

use proc_macro2::Span;
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{Attribute, ForeignItem, ImplItem, Item, TraitItem};

mod cfg;

const PRODUCTION_LINE_LIMIT: usize = 1_000;
const INLINE_TEST_LINE_LIMIT: usize = 300;
const SECONDS_PER_DAY: u64 = 60 * 60 * 24;
const REFRESH_ISSUE_STATE_SCRIPT: &str = "scripts/refresh-line-cap-issue-state.sh";

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
    // Optional so a caller can measure a tree without an issue-state snapshot.
    // `scripts/check-line-cap.sh` always supplies it, so the checked-in gate
    // always validates citations; omitting it here only skips that validation.
    issue_state: Option<PathBuf>,
    report: bool,
}

pub(crate) fn run(args: impl Iterator<Item = String>) -> Result<(), String> {
    let args = args.collect::<Vec<_>>();
    if args
        .iter()
        .any(|argument| argument == "-h" || argument == "--help")
    {
        println!(
            "Usage: cargo run -q -p conary-xtask -- line-cap --allowlist <path> --issue-state <path> [--root <path>] [--report]"
        );
        println!(
            "  --issue-state <path>  checked-in snapshot (no network I/O) that records every cited issue as OPEN"
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
    let mut used_allowlist_entries = BTreeSet::new();
    let mut errors = Vec::new();

    if let Some(issue_state_path) = &options.issue_state {
        let issue_state = read_issue_state(issue_state_path)?;
        if let Err(error) =
            validate_allowlist_freshness(&options.allowlist, issue_state_path, &issue_state)
        {
            errors.push(error);
        }
        validate_allowlist_issue_state(&allowlist, issue_state_path, &issue_state, &mut errors);
    }

    for path in scan.files {
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
        let metrics = match analyze_source(&source) {
            Ok(metrics) => metrics,
            Err(error) => {
                errors.push(format!("failed to parse {relative}: {error}"));
                continue;
            }
        };

        if excluded_test_file(relative_path) {
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
        let mut issue_state = None;
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
                "--issue-state" => {
                    issue_state =
                        Some(PathBuf::from(args.next().ok_or_else(|| {
                            "--issue-state requires a path".to_string()
                        })?));
                }
                "--report" => report = true,
                _ => return Err(format!("unknown line-cap argument: {argument}")),
            }
        }

        let allowlist = allowlist.ok_or_else(|| "--allowlist requires a path".to_string())?;
        Ok(Self {
            root,
            allowlist,
            issue_state,
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

fn positive_number(value: &str) -> Option<u64> {
    value.parse::<u64>().ok().filter(|number| *number > 0)
}

fn issue_number(value: &str) -> Option<u64> {
    value.strip_prefix('#').and_then(positive_number)
}

fn valid_issue(value: &str) -> bool {
    issue_number(value).is_some()
}

/// GitHub state of one cited issue, as recorded in the checked-in snapshot.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum IssueState {
    Open,
    Closed,
}

impl IssueState {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "OPEN" => Some(Self::Open),
            "CLOSED" => Some(Self::Closed),
            _ => None,
        }
    }
}

/// Checked-in issue state for every allowlist citation.
///
/// The gate performs no network I/O: `scripts/refresh-line-cap-issue-state.sh`
/// is the only producer of this snapshot and the only line-cap step that talks
/// to GitHub.
#[derive(Debug)]
struct IssueStateSnapshot {
    refreshed: String,
    refreshed_day: i64,
    states: BTreeMap<u64, IssueState>,
}

fn read_issue_state(path: &Path) -> Result<IssueStateSnapshot, String> {
    let contents = fs::read_to_string(path).map_err(|error| {
        format!(
            "line-cap issue-state snapshot not found: {}: {error}",
            path.display()
        )
    })?;
    parse_issue_state(&contents, path)
}

fn parse_issue_state(contents: &str, path: &Path) -> Result<IssueStateSnapshot, String> {
    let mut refreshed = None;
    let mut states = BTreeMap::new();

    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // `#` lines are comments, but a data line may carry the issue sigil
        // (`#154 OPEN`); the checked-in snapshot writes it bare (`154 OPEN`).
        let data = match line.strip_prefix('#') {
            Some(rest) if !rest.trim_start().starts_with(|c: char| c.is_ascii_digit()) => {
                if let Some(value) = rest.trim().strip_prefix("refreshed:") {
                    if refreshed.is_some() {
                        return Err(format!(
                            "duplicate refreshed date in issue-state snapshot {}",
                            path.display()
                        ));
                    }
                    refreshed = Some(value.trim().to_string());
                }
                continue;
            }
            Some(rest) => rest,
            None => line,
        };
        let fields = data.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 2 {
            return Err(format!(
                "invalid issue-state entry in {} (expected '<issue> <STATE>'): {line}",
                path.display()
            ));
        }
        let Some(number) = positive_number(fields[0]) else {
            return Err(format!(
                "invalid issue-state entry in {} (expected '<issue> <STATE>'): {line}",
                path.display()
            ));
        };
        let Some(state) = IssueState::parse(fields[1]) else {
            return Err(format!(
                "invalid issue state in {} (expected OPEN or CLOSED): {line}",
                path.display()
            ));
        };
        if states.insert(number, state).is_some() {
            return Err(format!(
                "duplicate issue-state entry in {}: #{number}",
                path.display()
            ));
        }
    }

    let refreshed = refreshed.ok_or_else(|| {
        format!(
            "issue-state snapshot {} has no 'refreshed: <YYYY-MM-DD>' line",
            path.display()
        )
    })?;
    let refreshed_day = parse_refreshed_day(&refreshed).map_err(|error| {
        format!(
            "issue-state snapshot {} has an invalid refreshed date `{refreshed}`: {error}",
            path.display()
        )
    })?;
    Ok(IssueStateSnapshot {
        refreshed,
        refreshed_day,
        states,
    })
}

fn parse_refreshed_day(value: &str) -> Result<i64, String> {
    let parts = value.split('-').collect::<Vec<_>>();
    let [year, month, day] = parts.as_slice() else {
        return Err("expected YYYY-MM-DD".to_string());
    };
    if year.len() != 4 || month.len() != 2 || day.len() != 2 {
        return Err("expected YYYY-MM-DD".to_string());
    }
    let (Ok(year), Ok(month), Ok(day)) = (
        year.parse::<i64>(),
        month.parse::<i64>(),
        day.parse::<i64>(),
    ) else {
        return Err("expected YYYY-MM-DD".to_string());
    };
    if !(1..=12).contains(&month) {
        return Err(format!("month out of range: {month}"));
    }
    let last_day = days_in_month(year, month);
    if !(1..=last_day).contains(&day) {
        return Err(format!("day out of range for {year:04}-{month:02}"));
    }
    Ok(days_from_civil(year, month, day))
}

fn leap_year(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Days from 1970-01-01 (Howard Hinnant's `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_index = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The snapshot cannot silently rot: any allowlist change must be followed by a
/// refresh, because freshly cited issues are exactly what the snapshot cannot know.
fn validate_allowlist_freshness(
    allowlist: &Path,
    snapshot_path: &Path,
    snapshot: &IssueStateSnapshot,
) -> Result<(), String> {
    let metadata = fs::metadata(allowlist).map_err(|error| {
        format!(
            "cannot read line-cap allowlist metadata {}: {error}",
            allowlist.display()
        )
    })?;
    let modified = metadata.modified().map_err(|error| {
        format!(
            "cannot read line-cap allowlist modification time {}: {error}",
            allowlist.display()
        )
    })?;
    let elapsed = modified.duration_since(UNIX_EPOCH).map_err(|error| {
        format!(
            "line-cap allowlist {} was modified before the Unix epoch: {error}",
            allowlist.display()
        )
    })?;
    let modified_day = (elapsed.as_secs() / SECONDS_PER_DAY) as i64;
    if modified_day <= snapshot.refreshed_day {
        return Ok(());
    }
    // A fresh checkout stamps every file with the checkout time, so the
    // allowlist's mtime alone cannot distinguish "edited after the snapshot"
    // from "checked out today". Compare the two files directly: a snapshot
    // written after the allowlist in the same checkout is fresh, and an
    // allowlist edited after the snapshot still fails.
    let snapshot_modified = fs::metadata(snapshot_path)
        .and_then(|metadata| metadata.modified())
        .map_err(|error| {
            format!(
                "cannot read line-cap issue-state snapshot modification time {}: {error}",
                snapshot_path.display()
            )
        })?;
    if snapshot_modified >= modified {
        return Ok(());
    }
    Err(format!(
        "line-cap allowlist {} is newer than issue-state snapshot {} (refreshed: {}); run {REFRESH_ISSUE_STATE_SCRIPT}",
        allowlist.display(),
        snapshot_path.display(),
        snapshot.refreshed
    ))
}

fn validate_allowlist_issue_state(
    allowlist: &BTreeMap<String, String>,
    snapshot_path: &Path,
    snapshot: &IssueStateSnapshot,
    errors: &mut Vec<String>,
) {
    for (path, issue) in allowlist {
        let Some(number) = issue_number(issue) else {
            continue;
        };
        match snapshot.states.get(&number) {
            Some(IssueState::Open) => {}
            Some(IssueState::Closed) => errors.push(format!(
                "allowlist entry {path} cites {issue}; snapshot records {issue} as CLOSED in {}",
                snapshot_path.display()
            )),
            None => errors.push(format!(
                "allowlist entry {path} cites {issue}, which is absent from issue-state snapshot {}",
                snapshot_path.display()
            )),
        }
    }
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
