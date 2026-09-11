// crates/conary-xtask/src/line_cap/tests.rs

use super::*;
use std::fs::File;
use std::process;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A throwaway tree so module resolution is exercised against real files,
/// exactly as rustc and the scanner see them.
struct FixtureRoot {
    path: PathBuf,
}

impl FixtureRoot {
    fn new(label: &str) -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after the epoch")
            .as_nanos();
        let path = env::temp_dir().join(format!(
            "conary-line-cap-{label}-{}-{unique}",
            process::id()
        ));
        fs::create_dir_all(&path).expect("fixture root is creatable");
        Self { path }
    }

    fn write(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.path.join(relative);
        fs::create_dir_all(path.parent().expect("fixture file has a parent"))
            .expect("fixture directory is creatable");
        fs::write(&path, contents).expect("fixture file is writable");
        path
    }

    /// Every `.rs` file below the fixture, as the scanner collects them.
    fn scanned(&self) -> BTreeSet<PathBuf> {
        let mut files = Vec::new();
        collect_rust_files(&self.path, &mut files).expect("fixture tree is readable");
        files.into_iter().collect()
    }

    /// Resolve a parent file's out-of-line children, as fixture-relative
    /// text, so an assertion reads like the repository path it mirrors.
    fn resolve(&self, parent: &str) -> Vec<String> {
        let path = self.path.join(parent);
        let source = fs::read_to_string(&path).expect("fixture parent is readable");
        let mut resolved = resolve_child_modules(&path, &source, &self.scanned())
            .into_iter()
            .map(|child| {
                path_text(
                    child
                        .strip_prefix(&self.path)
                        .expect("a resolved child stays inside the fixture"),
                )
            })
            .collect::<Vec<_>>();
        resolved.sort();
        resolved
    }
}

impl Drop for FixtureRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn resolves_the_declaration_forms_used_in_this_repository() {
    let fixture = FixtureRoot::new("forms");

    // `#[path = "qemu/tests.rs"] mod tests;` under a non-`mod.rs` parent,
    // exactly as apps/conary-test/src/engine/qemu.rs declares it.
    fixture.write(
        "crates/engine/src/qemu.rs",
        "#[cfg(test)]\n#[path = \"qemu/tests.rs\"]\nmod tests;\n",
    );
    fixture.write("crates/engine/src/qemu/tests.rs", "fn helper() {}\n");

    // A plain `mod tests;` under a non-`mod.rs` parent, exactly as
    // apps/remi/src/server/catalog_authority.rs declares it, plus that
    // file's nested `#[path = "catalog_authority/tests/test_support.rs"]`.
    // The child directory is `<dir>/<stem>/`, never `<dir>/`.
    fixture.write(
        "crates/engine/src/catalog_authority.rs",
        concat!(
            "#[cfg(test)]\n",
            "mod tests;\n",
            "#[cfg(test)]\n",
            "#[path = \"catalog_authority/tests/test_support.rs\"]\n",
            "pub(crate) mod test_support;\n",
        ),
    );
    fixture.write("crates/engine/src/catalog_authority/tests.rs", "");
    fixture.write(
        "crates/engine/src/catalog_authority/tests/test_support.rs",
        "",
    );

    // A plain `mod tests;` under a `mod.rs` parent, exactly as
    // crates/conary-core/src/repository/catalog/parity/rpm/mod.rs declares
    // it. A `mod.rs` parent owns its own directory instead.
    fixture.write(
        "crates/engine/src/parity/rpm/mod.rs",
        "mod ffi;\nmod resolution;\n#[cfg(test)]\nmod tests;\n",
    );
    fixture.write("crates/engine/src/parity/rpm/ffi.rs", "");
    fixture.write("crates/engine/src/parity/rpm/resolution/mod.rs", "");
    fixture.write("crates/engine/src/parity/rpm/tests.rs", "");

    assert_eq!(
        fixture.resolve("crates/engine/src/qemu.rs"),
        ["crates/engine/src/qemu/tests.rs"]
    );
    assert_eq!(
        fixture.resolve("crates/engine/src/catalog_authority.rs"),
        [
            "crates/engine/src/catalog_authority/tests.rs",
            "crates/engine/src/catalog_authority/tests/test_support.rs",
        ]
    );
    assert_eq!(
        fixture.resolve("crates/engine/src/parity/rpm/mod.rs"),
        [
            "crates/engine/src/parity/rpm/ffi.rs",
            "crates/engine/src/parity/rpm/resolution/mod.rs",
            "crates/engine/src/parity/rpm/tests.rs",
        ]
    );
}

#[test]
fn resolves_a_path_attribute_under_a_mod_rs_parent() {
    let fixture = FixtureRoot::new("mod-rs-path");
    // `#[path]` is relative to the containing file's directory, which for a
    // `mod.rs` parent is the same directory a plain `mod tests;` searches.
    fixture.write(
        "crates/engine/src/rpm/mod.rs",
        "#[cfg(test)]\n#[path = \"tests.rs\"]\nmod tests;\n",
    );
    fixture.write("crates/engine/src/rpm/tests.rs", "");
    assert_eq!(
        fixture.resolve("crates/engine/src/rpm/mod.rs"),
        ["crates/engine/src/rpm/tests.rs"]
    );
}

#[test]
fn normalizes_parent_components_in_a_path_attribute() {
    let fixture = FixtureRoot::new("normalize");
    fixture.write(
        "crates/engine/src/nested/child.rs",
        "#[path = \"../shared/helper.rs\"]\nmod helper;\n",
    );
    fixture.write("crates/engine/src/shared/helper.rs", "");
    assert_eq!(
        fixture.resolve("crates/engine/src/nested/child.rs"),
        ["crates/engine/src/shared/helper.rs"]
    );
}

#[test]
fn prefers_the_flat_file_and_drops_unresolved_or_inline_declarations() {
    let fixture = FixtureRoot::new("precedence");
    fixture.write(
        "crates/engine/src/parent.rs",
        concat!(
            "mod flat;\n",
            "mod directory;\n",
            "mod missing;\n",
            "mod inline { mod nested; }\n",
        ),
    );
    fixture.write("crates/engine/src/parent/flat.rs", "");
    fixture.write("crates/engine/src/parent/flat/mod.rs", "");
    fixture.write("crates/engine/src/parent/directory/mod.rs", "");
    assert_eq!(
        fixture.resolve("crates/engine/src/parent.rs"),
        [
            "crates/engine/src/parent/directory/mod.rs",
            "crates/engine/src/parent/flat.rs",
        ]
    );
}

#[test]
fn attributes_extracted_sibling_mass_to_the_declaring_parent() {
    let fixture = FixtureRoot::new("attribution");
    let source = "#[cfg(test)]\n#[path = \"extracting/tests.rs\"]\nmod tests;\n";
    let parent = fixture.write("crates/engine/src/extracting.rs", source);
    fixture.write(
        "crates/engine/src/extracting/tests.rs",
        "fn one() {}\nfn two() {}\n",
    );

    let children = resolve_child_modules(&parent, source, &fixture.scanned());
    let mut measured = MeasuredFiles::default();
    let attribution = sibling_attribution(&children, &mut measured);
    assert_eq!(
        attribution,
        SiblingAttribution {
            siblings: 1,
            sibling_lines: 2,
        }
    );
    assert_eq!(
        report_row(
            "crates/engine/src/extracting.rs",
            analyze_source(source).unwrap(),
            attribution,
        ),
        concat!(
            "crates/engine/src/extracting.rs\ttotal=3\tproduction=0\tinline_test=3",
            "\tsiblings=1\tsibling_tests=2\treduction=2",
        )
    );
}

#[test]
fn omits_sibling_fields_when_no_child_module_resolves() {
    let fixture = FixtureRoot::new("no-siblings");
    let source = "mod missing;\n#[cfg(test)]\nmod tests { fn helper() {} }\n";
    let parent = fixture.write("crates/engine/src/inline.rs", source);

    assert!(
        resolve_child_modules(&parent, source, &fixture.scanned()).is_empty(),
        "an unresolvable declaration is not a sibling"
    );
    assert_eq!(
        report_row(
            "crates/engine/src/inline.rs",
            analyze_source(source).unwrap(),
            SiblingAttribution::default(),
        ),
        "crates/engine/src/inline.rs\ttotal=3\tproduction=1\tinline_test=2"
    );
}
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
fn a_cfg_gated_include_site_gates_the_included_file() {
    let classified = classify(&[
        (
            "crates/x/src/owner.rs",
            "#[cfg(test)]\ninclude!(\"tests.rs\");\n",
        ),
        ("crates/x/src/tests.rs", "fn helper() {}\n"),
    ]);
    assert_eq!(
        gate(&classified, "crates/x/src/tests.rs"),
        ExemptionGate::TestGated
    );

    let classified = classify(&[
        ("crates/x/src/owner.rs", "include!(\"tests.rs\");\n"),
        ("crates/x/src/tests.rs", "fn production() {}\n"),
    ]);
    assert_eq!(
        gate(&classified, "crates/x/src/tests.rs"),
        ExemptionGate::Ungated
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

#[test]
fn a_test_gated_declaring_file_gates_its_own_declarations() {
    let classified = classify(&[
        ("crates/x/src/lib.rs", "#[cfg(test)]\nmod suite;\n"),
        ("crates/x/src/suite.rs", "mod tests;\n"),
        ("crates/x/src/suite/tests.rs", "fn helper() {}\n"),
    ]);
    assert_eq!(
        gate(&classified, "crates/x/src/suite/tests.rs"),
        ExemptionGate::TestGated
    );
}

#[test]
fn cargo_integration_test_targets_are_test_gated() {
    let classified = classify(&[
        ("crates/x/tests/query.rs", "mod common;\n"),
        ("crates/x/tests/common/mod.rs", "pub fn helper() {}\n"),
    ]);
    assert_eq!(
        gate(&classified, "crates/x/tests/query.rs"),
        ExemptionGate::TestGated
    );
    // Reached through the target root's own directory, not a sibling stem
    // directory, because the target root is a crate root.
    assert_eq!(
        gate(&classified, "crates/x/tests/common/mod.rs"),
        ExemptionGate::TestGated
    );
}

#[test]
fn declaration_forms_resolve_the_way_rust_loads_them() {
    assert_eq!(
        relative_module_directory(Path::new("crates/x/src/foo.rs")),
        PathBuf::from("crates/x/src/foo")
    );
    assert_eq!(
        relative_module_directory(Path::new("crates/x/src/foo/mod.rs")),
        PathBuf::from("crates/x/src/foo")
    );
    assert_eq!(
        relative_module_directory(Path::new("crates/x/src/lib.rs")),
        PathBuf::from("crates/x/src")
    );
    // A cargo integration test target is a crate root, so it owns its
    // directory instead of a sibling named after its stem.
    assert_eq!(
        relative_module_directory(Path::new("crates/x/tests/query.rs")),
        PathBuf::from("crates/x/tests")
    );

    // `Foo.rs` declares `mod tests;` as `Foo/tests.rs`.
    let (_, declarations) = declarations_of(&[("crates/x/src/foo.rs", "mod tests;\n")]);
    assert!(declarations.contains_key("crates/x/src/foo/tests.rs"));
    assert!(declarations.contains_key("crates/x/src/foo/tests/mod.rs"));
    assert!(!declarations.contains_key("crates/x/src/tests.rs"));

    // `Foo/mod.rs` declares `mod tests;` as `Foo/tests.rs`.
    let (_, declarations) = declarations_of(&[("crates/x/src/foo/mod.rs", "mod tests;\n")]);
    assert!(declarations.contains_key("crates/x/src/foo/tests.rs"));
    assert!(declarations.contains_key("crates/x/src/foo/tests/mod.rs"));

    // A crate root declares `mod tests;` beside itself.
    let (_, declarations) = declarations_of(&[("crates/x/src/lib.rs", "mod tests;\n")]);
    assert!(declarations.contains_key("crates/x/src/tests.rs"));
    assert!(declarations.contains_key("crates/x/src/tests/mod.rs"));

    assert_eq!(
        cargo_target_root(Path::new("crates/x/tests/query.rs")),
        Some(CargoTarget::Test)
    );
    assert_eq!(
        cargo_target_root(Path::new("crates/x/tests/common/mod.rs")),
        None
    );
    assert_eq!(cargo_target_root(Path::new("crates/x/src/tests.rs")), None);
    assert_eq!(
        cargo_target_root(Path::new("crates/x/benches/throughput.rs")),
        Some(CargoTarget::Other)
    );
    assert_eq!(
        cargo_target_root(Path::new("crates/x/src/bin/tool.rs")),
        Some(CargoTarget::Other)
    );
}

#[test]
fn declaration_predicates_gate_an_exempt_file() {
    for declaration in [
        "#[cfg(test)]\nmod tests;\n",
        "#[cfg(all(test, feature = \"fixture\"))]\nmod tests;\n",
        "#[cfg_attr(all(), cfg(test))]\nmod tests;\n",
        "#[cfg(not(not(test)))]\nmod tests;\n",
    ] {
        let classified = classify(&[
            ("crates/x/src/owner.rs", declaration),
            ("crates/x/src/owner/tests.rs", "fn helper() {}\n"),
        ]);
        assert_eq!(
            gate(&classified, "crates/x/src/owner/tests.rs"),
            ExemptionGate::TestGated,
            "{declaration}"
        );
    }
    // Reachable from a non-test build, so the file stays production code.
    let classified = classify(&[
        (
            "crates/x/src/owner.rs",
            "#[cfg(any(test, feature = \"fixture\"))]\nmod tests;\n",
        ),
        ("crates/x/src/owner/tests.rs", "fn production() {}\n"),
    ]);
    assert_eq!(
        gate(&classified, "crates/x/src/owner/tests.rs"),
        ExemptionGate::Ungated
    );
}

#[test]
fn exempt_file_gated_by_its_own_inner_attribute_is_test_gated() {
    for source in [
        "#![cfg(test)]\nfn helper() {}\n",
        "#![cfg(all(test, feature = \"fixture\"))]\nfn helper() {}\n",
    ] {
        let classified = classify(&[("crates/x/src/tests/support.rs", source)]);
        assert_eq!(
            gate(&classified, "crates/x/src/tests/support.rs"),
            ExemptionGate::TestGated,
            "{source}"
        );
    }
    // A file-level gate that holds in a non-test build leaves production.
    let classified = classify(&[(
        "crates/x/src/tests/support.rs",
        "#![cfg(any(test, feature = \"fixture\"))]\nfn production() {}\n",
    )]);
    assert_eq!(
        gate(&classified, "crates/x/src/tests/support.rs"),
        ExemptionGate::Ungated
    );
}

#[test]
fn exempt_file_with_a_test_gated_declaring_site_is_test_gated() {
    let classified = classify(&[
        ("crates/x/src/owner.rs", "#[cfg(test)]\nmod tests;\n"),
        ("crates/x/src/owner/tests.rs", "fn helper() {}\n"),
    ]);
    assert_eq!(
        gate(&classified, "crates/x/src/owner/tests.rs"),
        ExemptionGate::TestGated
    );
}

#[test]
fn exempt_file_with_no_gate_anywhere_is_ungated() {
    // Never declared at all.
    let classified = classify(&[("crates/x/src/tests.rs", "fn production() {}\n")]);
    assert_eq!(
        gate(&classified, "crates/x/src/tests.rs"),
        ExemptionGate::Ungated
    );
    // Declared, but the declaring site is not test-gated: the masking case.
    let classified = classify(&[
        ("crates/x/src/owner.rs", "mod tests;\n"),
        ("crates/x/src/owner/tests.rs", "fn production() {}\n"),
    ]);
    assert_eq!(
        gate(&classified, "crates/x/src/owner/tests.rs"),
        ExemptionGate::Ungated
    );
}

#[test]
fn include_chains_inherit_the_including_files_gate() {
    // The shape of crates/conary-core/src/repository/sync.rs: a test-gated
    // include whose text opens `mod tests` and includes a deeper file from
    // inside that module.
    let classified = classify(&[
        (
            "crates/x/src/sync.rs",
            "#[cfg(test)]\ninclude!(\"sync/tests.rs\");\n",
        ),
        (
            "crates/x/src/sync/tests.rs",
            "mod tests {\n    include!(\"tests/native.rs\");\n}\n",
        ),
        ("crates/x/src/sync/tests/native.rs", "fn helper() {}\n"),
    ]);
    assert_eq!(
        gate(&classified, "crates/x/src/sync/tests.rs"),
        ExemptionGate::TestGated
    );
    assert_eq!(
        gate(&classified, "crates/x/src/sync/tests/native.rs"),
        ExemptionGate::TestGated
    );
}

#[test]
fn inline_module_gates_flow_to_their_declared_files() {
    let classified = classify(&[
        (
            "crates/x/src/lib.rs",
            "mod outer {\n    #[cfg(test)]\n    mod tests;\n}\n",
        ),
        ("crates/x/src/outer/tests.rs", "fn helper() {}\n"),
    ]);
    assert_eq!(
        gate(&classified, "crates/x/src/outer/tests.rs"),
        ExemptionGate::TestGated
    );

    let classified = classify(&[
        (
            "crates/x/src/lib.rs",
            "#[cfg(test)]\nmod outer {\n    mod tests;\n}\n",
        ),
        ("crates/x/src/outer/tests.rs", "fn helper() {}\n"),
    ]);
    assert_eq!(
        gate(&classified, "crates/x/src/outer/tests.rs"),
        ExemptionGate::TestGated
    );
}

#[test]
fn one_ungated_declaring_site_keeps_the_file_in_production() {
    let classified = classify(&[
        ("crates/x/src/lib.rs", "#[cfg(test)]\nmod tests;\n"),
        (
            "crates/x/src/other.rs",
            "#[path = \"tests.rs\"]\nmod tests;\n",
        ),
        ("crates/x/src/tests.rs", "fn production() {}\n"),
    ]);
    assert_eq!(
        gate(&classified, "crates/x/src/tests.rs"),
        ExemptionGate::Ungated
    );
}

#[test]
fn path_attribute_resolves_relative_to_the_containing_file() {
    // The real declaring site of apps/remi/src/server/catalog_authority/tests/test_support.rs.
    let classified = classify(&[
        (
            "apps/remi/src/server/catalog_authority.rs",
            "#[cfg(test)]\n#[path = \"catalog_authority/tests/test_support.rs\"]\npub(crate) mod test_support;\n",
        ),
        (
            "apps/remi/src/server/catalog_authority/tests/test_support.rs",
            "fn helper() {}\n",
        ),
    ]);
    assert_eq!(
        gate(
            &classified,
            "apps/remi/src/server/catalog_authority/tests/test_support.rs"
        ),
        ExemptionGate::TestGated
    );

    // The real `#[path]` value of apps/conary/src/commands/test_helpers.rs
    // crosses out of its own directory into the package's tests/ tree.
    let classified = classify(&[
        (
            "apps/conary/src/commands/mod.rs",
            "#[cfg(test)]\npub(crate) mod test_helpers;\n",
        ),
        (
            "apps/conary/src/commands/test_helpers.rs",
            "#[path = \"../../tests/common/update_ccs.rs\"]\npub(crate) mod update_ccs;\n",
        ),
        (
            "apps/conary/tests/common/update_ccs.rs",
            "pub(crate) fn helper() {}\n",
        ),
    ]);
    assert_eq!(
        gate(&classified, "apps/conary/tests/common/update_ccs.rs"),
        ExemptionGate::TestGated
    );
}

// --- helpers for the exemption-classification tests (#997) ---

fn declarations_of(
    sources: &[(&str, &str)],
) -> (
    BTreeMap<String, bool>,
    BTreeMap<String, Vec<ModuleDeclaration>>,
) {
    let mut declarations = BTreeMap::new();
    let mut gates = BTreeMap::new();
    for (relative, source) in sources {
        let syntax = syn::parse_file(source).unwrap();
        let path = Path::new(relative);
        collect_module_declarations(&syntax, path, &mut declarations);
        collect_include_declarations(&syntax, path, &mut declarations);
        gates.insert(relative.to_string(), file_level_test_gate(&syntax, path));
    }
    (gates, declarations)
}

fn classify(sources: &[(&str, &str)]) -> BTreeMap<String, ExemptionGate> {
    let (gates, declarations) = declarations_of(sources);
    let resolved = resolve_test_gates(&gates, &declarations);
    let mut classified = BTreeMap::new();
    for (relative, _) in sources {
        if !excluded_test_file(Path::new(relative)) {
            continue;
        }
        let gate = exemption_gate(resolved.get(*relative).copied().unwrap_or(false));
        classified.insert(relative.to_string(), gate);
    }
    classified
}

fn gate(classified: &BTreeMap<String, ExemptionGate>, path: &str) -> ExemptionGate {
    *classified
        .get(path)
        .unwrap_or_else(|| panic!("{path} was not classified as exempt"))
}
