// crates/conary-xtask/src/line_cap/tests.rs

use super::*;
use std::fs::File;
use std::process;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

#[test]
fn counts_the_union_of_typed_test_item_spans() {
    let source = r#"fn production() {}
#[cfg(test)]
/* retained inside the test span */
/// test helper
fn helper() {
    assert!(true);
}
fn middle() {}
#[cfg(all(test, feature = "fixture"))]
const FIXTURE: &str = "value";
"#;

    assert_eq!(
        analyze_source(source).unwrap(),
        FileMetrics {
            total_lines: 10,
            production_lines: 2,
            inline_test_lines: 8,
        }
    );
}

#[test]
fn rejects_malformed_rust() {
    assert!(analyze_source("fn broken( {").is_err());
}

#[test]
fn counts_standalone_and_conditionally_annotated_tests() {
    for annotation in [
        "test",
        "tokio::test(flavor = \"current_thread\")",
        "cfg_attr(all(), test)",
        "cfg_attr(all(), allow(dead_code), cfg_attr(all(), tokio::test))",
        // Annotated in every non-test build, so no production build keeps it.
        "cfg_attr(not(test), test)",
    ] {
        let source = format!("#[{annotation}]\nasync fn example() {{}}\n");
        assert_eq!(
            analyze_source(&source).unwrap().inline_test_lines,
            2,
            "{annotation}"
        );
    }
    // A conditional annotation whose condition can be false in a non-test
    // build leaves an ordinary function that production compiles.
    for annotation in [
        "cfg_attr(test, test)",
        "cfg_attr(all(test, feature = \"x\"), tokio::test)",
        "cfg_attr(test, allow(dead_code), cfg_attr(feature = \"x\", test))",
        "cfg_attr(feature = \"x\", test)",
        "cfg_attr(test, allow(dead_code))",
        "cfg_attr(all(test, not(test)), test)",
        "test_helper",
    ] {
        let source = format!("#[{annotation}]\nfn example() {{}}\n");
        assert_eq!(
            analyze_source(&source).unwrap().production_lines,
            2,
            "{annotation}"
        );
    }
    let source = "#[cfg(test)]\nmod tests {\n    #[test]\n    fn nested() {}\n}\n#[test]\nfn standalone() {}\n";
    assert_eq!(analyze_source(source).unwrap().inline_test_lines, 7);
}

#[test]
fn cfg_feature_named_test_is_not_the_test_predicate() {
    let source = "#[cfg(feature = \"test\")]\nfn production() {}\n";
    assert_eq!(analyze_source(source).unwrap().production_lines, 2);
}

#[test]
fn cfg_attr_gating_preserves_the_inactive_production_branch() {
    for (attribute, test_only) in [
        ("cfg_attr(all(), cfg(test))", true),
        ("cfg_attr(feature = \"x\", cfg(test))", false),
        (
            "cfg_attr(all(), cfg_attr(feature = \"x\", cfg(test)))",
            false,
        ),
        ("cfg_attr(all(), cfg_attr(all(), cfg(test)))", true),
        ("cfg_attr(any(), cfg(test))", false),
        ("cfg_attr(all(), allow(dead_code), cfg(test))", true),
    ] {
        let source = format!("#[{attribute}]\nfn example() {{}}\n");
        assert_eq!(
            analyze_source(&source).unwrap().inline_test_lines,
            if test_only { 2 } else { 0 },
            "{attribute}"
        );
    }
}

#[test]
fn evaluates_cfg_test_polarity() {
    let source = r#"#[cfg(not(test))]
fn production_when_not_testing() {}
#[cfg(any(test, feature = "fixture"))]
fn production_with_feature() {}
#[cfg(all(test, feature = "fixture"))]
fn test_only() {}
#[cfg(not(not(test)))]
fn nested_test_only() {}
#[cfg(any(not(test), all(test, feature = "fixture")))]
fn nested_production() {}
"#;

    assert_eq!(
        analyze_source(source).unwrap(),
        FileMetrics {
            total_lines: 10,
            production_lines: 6,
            inline_test_lines: 4,
        }
    );
}

#[test]
fn cfg_symbols_preserve_negation_and_repeated_atom_identity() {
    for (predicate, test_only) in [
        ("any(test, not(unix))", false),
        ("all(test, not(unix))", true),
        ("any(test, all(unix, not(unix)))", true),
        ("all(test, unix, not(unix))", false),
        ("any(test, not(feature = \"x\"))", false),
        ("all(test, not(feature = \"x\"))", true),
    ] {
        let source = format!("#[cfg({predicate})]\nfn example() {{}}\n");
        assert_eq!(
            analyze_source(&source).unwrap().inline_test_lines,
            if test_only { 2 } else { 0 },
            "{predicate}"
        );
    }
    let source = "#[cfg(any(test, unix))]\n#[cfg(any(test, not(unix)))]\nfn helper() {}\n";
    assert_eq!(analyze_source(source).unwrap().inline_test_lines, 3);
}

#[test]
fn counts_fields_statements_and_expressions_as_test_regions() {
    let source = r#"struct Example {
    #[cfg(test)]
    helper: usize,
}
fn example() {
    #[cfg(test)]
    let helper = 1;
    #[cfg(test)]
    {
        #[cfg(test)]
        let nested = 2;
    }
    #[cfg(test)]
    assert!(true);
    let value = Example {
        #[cfg(test)]
        helper: 3,
    };
}
enum Choice {
    #[cfg(test)]
    Test,
    Production,
}
"#;
    assert_eq!(
        analyze_source(source).unwrap(),
        FileMetrics {
            total_lines: 24,
            production_lines: 9,
            inline_test_lines: 15,
        }
    );
}

#[test]
fn counts_associated_items_without_double_counting_a_test_impl() {
    let source = r#"struct Example;
impl Example {
    #[cfg(test)]
    const FIXTURE: usize = 1;
    fn production() {}
}
#[cfg(test)]
impl Example {
    #[cfg(test)]
    fn nested_test_helper() {}
}
"#;

    assert_eq!(
        analyze_source(source).unwrap(),
        FileMetrics {
            total_lines: 11,
            production_lines: 4,
            inline_test_lines: 7,
        }
    );
}

#[test]
fn file_level_inner_cfg_test_owns_the_whole_file() {
    let source =
        "// crates/example/src/support.rs\n#![cfg(test)]\n\nfn helper() {}\nfn other() {}\n";
    assert_eq!(
        analyze_source(source).unwrap(),
        FileMetrics {
            total_lines: 5,
            production_lines: 0,
            inline_test_lines: 5,
        }
    );
    let source = "#![allow(dead_code)]\n#![cfg(not(test))]\nfn production() {}\n";
    assert_eq!(analyze_source(source).unwrap().production_lines, 3);
    let source = "#![cfg_attr(feature = \"x\", cfg(test))]\nfn production() {}\n";
    assert_eq!(analyze_source(source).unwrap().production_lines, 2);
}

#[test]
fn enclosing_cfg_predicates_narrow_child_classification() {
    // The module is production-capable (feature = "prod"), but its child can
    // only exist when `test` is set, so the child is inline-test code.
    let source = "#[cfg(any(test, feature = \"prod\"))]\nmod mixed {\n    #[cfg(not(feature = \"prod\"))]\n    fn test_only_child() {}\n    fn production_child() {}\n}\n";
    assert_eq!(
        analyze_source(source).unwrap(),
        FileMetrics {
            total_lines: 6,
            production_lines: 4,
            inline_test_lines: 2,
        }
    );
    // File-level constraints propagate the same way.
    let source = "#![cfg(any(test, feature = \"prod\"))]\n#[cfg(not(feature = \"prod\"))]\nfn test_only() {}\nfn production() {}\n";
    assert_eq!(analyze_source(source).unwrap().inline_test_lines, 2);
    // A child that widens nothing stays production under a production parent.
    let source = "#[cfg(feature = \"prod\")]\nmod prod {\n    #[cfg(unix)]\n    fn child() {}\n}\n";
    assert_eq!(analyze_source(source).unwrap().inline_test_lines, 0);
    // Constraints do not leak to siblings after leaving the parent.
    let source = "#[cfg(any(test, feature = \"prod\"))]\nmod mixed {\n    #[cfg(not(feature = \"prod\"))]\n    fn child() {}\n}\n#[cfg(not(feature = \"prod\"))]\nfn sibling() {}\n";
    assert_eq!(analyze_source(source).unwrap().inline_test_lines, 2);
}

#[test]
fn validates_repo_relative_path_comments() {
    let path = Path::new("crates/example/src/tests.rs");
    assert!(validate_path_comment("// crates/example/src/tests.rs\n", path).is_ok());
    assert!(
        validate_path_comment("// crates/wrong/src/tests.rs\n", path)
            .unwrap_err()
            .contains("expected `// crates/example/src/tests.rs`")
    );
    for source in [
        "",
        "fn example() {}\n",
        "// ordinary comment\n",
        "// example/src/tests.rs\n",
    ] {
        assert!(validate_path_comment(source, path).is_err());
    }
}

#[test]
fn classifies_declared_source_roots_by_policy() {
    assert_eq!(root_policy("apps"), Some(RootPolicy::Scanned));
    assert_eq!(root_policy("crates"), Some(RootPolicy::Scanned));
    assert_eq!(root_policy("fixture_pkg"), None);
    let Some(RootPolicy::VendorExcluded { reason }) = root_policy("third_party") else {
        panic!("third_party must be vendor-excluded, not scanned");
    };
    for vendored in ["aws-creds", "rust-s3", "resolvo"] {
        assert!(
            reason.contains(vendored),
            "vendor reason must name {vendored}: {reason}"
        );
    }
    assert!(reason.contains("[patch.crates-io]"), "{reason}");
    assert!(reason.contains("Cargo.toml"), "{reason}");
}

#[test]
fn rejects_undeclared_top_level_rust_roots() {
    let root = TempRoot::new("undeclared");
    let conary_file = root.path().join("crates/fixture/src/lib.rs");
    write_source(
        &conary_file,
        "// crates/fixture/src/lib.rs\nfn production() {}\n",
    );

    // A declared root contributes files without a classification error.
    let scan = rust_source_files(root.path()).unwrap();
    assert_eq!(scan.files, vec![conary_file]);
    assert!(undeclared_rust_roots(root.path()).unwrap().is_empty());

    // The same tree with a new top-level Rust directory fails loudly.
    write_source(
        &root.path().join("fixture_pkg/nested/deeper/lib.rs"),
        "fn undeclared() {}\n",
    );
    assert_eq!(undeclared_rust_roots(root.path()).unwrap(), ["fixture_pkg"]);
    let error = rust_source_files(root.path()).unwrap_err();
    assert!(
        error.contains("undeclared top-level Rust source root"),
        "{error}"
    );
    assert!(error.contains("fixture_pkg"), "{error}");
    assert!(error.contains("SOURCE_ROOTS"), "{error}");
}

#[test]
fn ignores_exempt_hidden_and_rust_free_top_level_directories() {
    let root = TempRoot::new("ignored");
    for name in ["target", "node_modules", ".git", ".worktrees", ".cache"] {
        write_source(
            &root.path().join(name).join("nested/generated.rs"),
            "fn generated() {}\n",
        );
    }
    write_source(&root.path().join("docs/readme.md"), "no Rust here\n");
    write_source(
        &root.path().join("recipes/nested/probe.rs.txt"),
        "no Rust\n",
    );
    assert_eq!(
        undeclared_rust_roots(root.path()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn vendor_excluded_roots_are_counted_but_not_measured() {
    let root = TempRoot::new("vendor");
    let conary_file = root.path().join("crates/fixture/src/lib.rs");
    write_source(
        &conary_file,
        "// crates/fixture/src/lib.rs\nfn production() {}\n",
    );
    // Over the production cap and missing its path comment: measured vendor
    // source would fail twice, so passing proves the exclusion is real.
    let vendor_source = "fn vendored() {}\n".repeat(PRODUCTION_LINE_LIMIT + 1);
    write_source(
        &root.path().join("third_party/vendored/src/over_cap.rs"),
        &vendor_source,
    );

    let scan = rust_source_files(root.path()).unwrap();
    assert_eq!(scan.files, vec![conary_file]);
    let coverage = source_roots_text(&scan.coverage);
    assert!(
        coverage.starts_with(
            "apps=0 files (scanned); crates=1 files (scanned); third_party=1 files (vendor-excluded: "
        ),
        "{coverage}"
    );

    let allowlist = root.path().join("allowlist.txt");
    write_source(&allowlist, "");
    let args = [
        "--root",
        root.path().to_str().unwrap(),
        "--allowlist",
        allowlist.to_str().unwrap(),
        "--report",
    ];
    assert_eq!(run(args.into_iter().map(String::from)), Ok(()));
}

fn root_policy(name: &str) -> Option<RootPolicy> {
    declared_source_root(name).map(|source_root| source_root.policy)
}

/// Write a fixture file, creating its parent directories.
fn write_source(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

/// A uniquely named temporary directory that removes itself.
struct TempRoot {
    path: PathBuf,
}

impl TempRoot {
    fn new(label: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = env::temp_dir().join(format!(
            "conary-xtask-line-cap-{}-{label}-{unique}",
            process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn parses_issue_state_snapshots() {
    let source = r#"# Issue state for scripts/line-cap-allowlist.txt entries.
# Regenerate with scripts/refresh-line-cap-issue-state.sh after any allowlist change.
# refreshed: 2026-09-12
#
# Format: #<issue> <STATE>
154 OPEN
852 OPEN
"#;
    let snapshot =
        parse_issue_state(source, Path::new("scripts/line-cap-issue-state.txt")).unwrap();
    assert_eq!(snapshot.refreshed, "2026-09-12");
    assert_eq!(snapshot.refreshed_day, days_from_civil(2026, 9, 12));
    assert_eq!(snapshot.states.get(&154), Some(&IssueState::Open));
    assert_eq!(snapshot.states.get(&852), Some(&IssueState::Open));
    assert_eq!(snapshot.states.get(&853), None);
    // The documented `#<issue>` sigil is accepted as well.
    let sigil = parse_issue_state(
        "# refreshed: 2026-09-12\n#154 OPEN\n",
        Path::new("scripts/line-cap-issue-state.txt"),
    )
    .unwrap();
    assert_eq!(sigil.states.get(&154), Some(&IssueState::Open));
}

#[test]
fn rejects_malformed_issue_state_snapshots() {
    let path = Path::new("scripts/line-cap-issue-state.txt");
    for (source, expected) in [
        ("154 OPEN\n", "has no 'refreshed"),
        (
            "# refreshed: 2026-09-12\n#154\n",
            "invalid issue-state entry",
        ),
        (
            "# refreshed: 2026-09-12\n#0 OPEN\n",
            "invalid issue-state entry",
        ),
        (
            "# refreshed: 2026-09-12\n154 OPEN extra\n",
            "invalid issue-state entry",
        ),
        (
            "# refreshed: 2026-09-12\n154 UNKNOWN\n",
            "invalid issue state",
        ),
        (
            "# refreshed: 2026-09-12\n154 OPEN\n154 OPEN\n",
            "duplicate issue-state entry",
        ),
        (
            "# refreshed: 2026-09-12\n# refreshed: 2026-09-13\n154 OPEN\n",
            "duplicate refreshed date",
        ),
        (
            "# refreshed: 2026-02-30\n154 OPEN\n",
            "invalid refreshed date",
        ),
        (
            "# refreshed: 2026-9-12\n154 OPEN\n",
            "invalid refreshed date",
        ),
        (
            "# refreshed: 2026-13-01\n154 OPEN\n",
            "invalid refreshed date",
        ),
    ] {
        let error = parse_issue_state(source, path).unwrap_err();
        assert!(error.contains(expected), "{source:?} -> {error}");
    }
}

#[test]
fn parses_calendar_dates_strictly() {
    assert_eq!(parse_refreshed_day("1970-01-01"), Ok(0));
    assert_eq!(parse_refreshed_day("2026-09-12"), Ok(20_708));
    assert_eq!(
        parse_refreshed_day("2024-02-29"),
        Ok(days_from_civil(2024, 2, 29))
    );
    for value in [
        "2023-02-29",
        "2026-00-10",
        "2026-12-32",
        "20260912",
        "2026-09-12T00:00:00Z",
    ] {
        assert!(parse_refreshed_day(value).is_err(), "{value}");
    }
}

#[test]
fn rejects_a_citation_the_snapshot_records_as_closed() {
    let allowlist = BTreeMap::from([("crates/demo/src/lib.rs".to_string(), "#814".to_string())]);
    let snapshot_path = Path::new("scripts/line-cap-issue-state.txt");
    let snapshot =
        parse_issue_state("# refreshed: 2026-09-12\n#814 CLOSED\n", snapshot_path).unwrap();
    let mut errors = Vec::new();
    validate_allowlist_issue_state(&allowlist, snapshot_path, &snapshot, &mut errors);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].contains("crates/demo/src/lib.rs"),
        "{}",
        errors[0]
    );
    assert!(
        errors[0].contains("snapshot records #814 as CLOSED"),
        "{}",
        errors[0]
    );
}

#[test]
fn rejects_a_citation_missing_from_the_snapshot() {
    let allowlist = BTreeMap::from([("crates/demo/src/lib.rs".to_string(), "#999".to_string())]);
    let snapshot_path = Path::new("scripts/line-cap-issue-state.txt");
    let snapshot =
        parse_issue_state("# refreshed: 2026-09-12\n#852 OPEN\n", snapshot_path).unwrap();
    let mut errors = Vec::new();
    validate_allowlist_issue_state(&allowlist, snapshot_path, &snapshot, &mut errors);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].contains("crates/demo/src/lib.rs"),
        "{}",
        errors[0]
    );
    assert!(errors[0].contains("#999"), "{}", errors[0]);
    assert!(
        errors[0].contains("absent from issue-state snapshot"),
        "{}",
        errors[0]
    );
}

#[test]
fn accepts_open_citations_covered_by_the_snapshot() {
    let allowlist = BTreeMap::from([
        ("crates/demo/src/lib.rs".to_string(), "#154".to_string()),
        ("crates/demo/src/other.rs".to_string(), "#852".to_string()),
    ]);
    let snapshot_path = Path::new("scripts/line-cap-issue-state.txt");
    let snapshot = parse_issue_state(
        "# refreshed: 2026-09-12\n#154 OPEN\n#852 OPEN\n",
        snapshot_path,
    )
    .unwrap();
    let mut errors = Vec::new();
    validate_allowlist_issue_state(&allowlist, snapshot_path, &snapshot, &mut errors);
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn rejects_an_allowlist_newer_than_the_snapshot_refresh_date() {
    let scratch = ScratchDir::new("stale-allowlist");
    let allowlist = scratch.path().join("allowlist.txt");
    fs::write(&allowlist, "crates/demo/src/lib.rs #123\n").unwrap();
    let snapshot_path = scratch.path().join("issue-state.txt");
    fs::write(&snapshot_path, "# refreshed: 1970-01-02\n#123 OPEN\n").unwrap();
    // Backdate the snapshot so the allowlist is genuinely the newer file. The
    // equal-mtime checkout case is covered by the next test.
    let day_ago = SystemTime::now() - Duration::from_secs(SECONDS_PER_DAY);
    File::options()
        .write(true)
        .open(&snapshot_path)
        .unwrap()
        .set_modified(day_ago)
        .unwrap();
    let snapshot =
        parse_issue_state("# refreshed: 1970-01-02\n#123 OPEN\n", &snapshot_path).unwrap();
    let error = validate_allowlist_freshness(&allowlist, &snapshot_path, &snapshot).unwrap_err();
    assert!(
        error.contains("run scripts/refresh-line-cap-issue-state.sh"),
        "{error}"
    );
    assert!(error.contains("refreshed: 1970-01-02"), "{error}");
    assert!(error.contains("allowlist.txt"), "{error}");
    assert!(error.contains("issue-state.txt"), "{error}");
}

#[test]
fn accepts_a_snapshot_checked_out_alongside_the_allowlist() {
    // A fresh checkout stamps every file with the checkout time, so the
    // allowlist's mtime can be far newer than the snapshot's `refreshed:` date
    // without anyone editing it. That must not fail CI, or every PR would fail
    // from the day after the snapshot was generated.
    let scratch = ScratchDir::new("fresh-checkout");
    let allowlist = scratch.path().join("allowlist.txt");
    fs::write(&allowlist, "crates/demo/src/lib.rs #123\n").unwrap();
    let snapshot_path = scratch.path().join("issue-state.txt");
    fs::write(&snapshot_path, "# refreshed: 1970-01-02\n#123 OPEN\n").unwrap();
    let later = SystemTime::now() + Duration::from_secs(1);
    File::options()
        .write(true)
        .open(&snapshot_path)
        .unwrap()
        .set_modified(later)
        .unwrap();
    let snapshot =
        parse_issue_state("# refreshed: 1970-01-02\n#123 OPEN\n", &snapshot_path).unwrap();
    assert_eq!(
        validate_allowlist_freshness(&allowlist, &snapshot_path, &snapshot),
        Ok(())
    );
}

#[test]
fn accepts_an_allowlist_older_than_the_snapshot_refresh_date() {
    let scratch = ScratchDir::new("fresh-allowlist");
    let allowlist = scratch.path().join("allowlist.txt");
    fs::write(&allowlist, "crates/demo/src/lib.rs #123\n").unwrap();
    let snapshot_path = scratch.path().join("issue-state.txt");
    let snapshot =
        parse_issue_state("# refreshed: 9999-12-31\n#123 OPEN\n", &snapshot_path).unwrap();
    validate_allowlist_freshness(&allowlist, &snapshot_path, &snapshot).unwrap();
}

struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new(name: &str) -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before the Unix epoch")
            .as_nanos();
        let path = env::temp_dir().join(format!(
            "conary-xtask-line-cap-{name}-{}-{unique}",
            process::id()
        ));
        fs::create_dir_all(&path).expect("create scratch dir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
