// crates/conary-xtask/src/line_cap/tests.rs

use super::*;

fn analyze(source: &str) -> syn::Result<FileMetrics> {
    Ok(measure_source(&syn::parse_file(source)?, source))
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
        analyze(source).unwrap(),
        FileMetrics {
            total_lines: 10,
            production_lines: 2,
            inline_test_lines: 8,
        }
    );
}

#[test]
fn rejects_malformed_rust() {
    assert!(syn::parse_file("fn broken( {").is_err());
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
            analyze(&source).unwrap().inline_test_lines,
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
            analyze(&source).unwrap().production_lines,
            2,
            "{annotation}"
        );
    }
    let source = "#[cfg(test)]\nmod tests {\n    #[test]\n    fn nested() {}\n}\n#[test]\nfn standalone() {}\n";
    assert_eq!(analyze(source).unwrap().inline_test_lines, 7);
}

#[test]
fn cfg_feature_named_test_is_not_the_test_predicate() {
    let source = "#[cfg(feature = \"test\")]\nfn production() {}\n";
    assert_eq!(analyze(source).unwrap().production_lines, 2);
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
            analyze(&source).unwrap().inline_test_lines,
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
        analyze(source).unwrap(),
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
            analyze(&source).unwrap().inline_test_lines,
            if test_only { 2 } else { 0 },
            "{predicate}"
        );
    }
    let source = "#[cfg(any(test, unix))]\n#[cfg(any(test, not(unix)))]\nfn helper() {}\n";
    assert_eq!(analyze(source).unwrap().inline_test_lines, 3);
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
        analyze(source).unwrap(),
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
        analyze(source).unwrap(),
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
        analyze(source).unwrap(),
        FileMetrics {
            total_lines: 5,
            production_lines: 0,
            inline_test_lines: 5,
        }
    );
    let source = "#![allow(dead_code)]\n#![cfg(not(test))]\nfn production() {}\n";
    assert_eq!(analyze(source).unwrap().production_lines, 3);
    let source = "#![cfg_attr(feature = \"x\", cfg(test))]\nfn production() {}\n";
    assert_eq!(analyze(source).unwrap().production_lines, 2);
}

#[test]
fn enclosing_cfg_predicates_narrow_child_classification() {
    // The module is production-capable (feature = "prod"), but its child can
    // only exist when `test` is set, so the child is inline-test code.
    let source = "#[cfg(any(test, feature = \"prod\"))]\nmod mixed {\n    #[cfg(not(feature = \"prod\"))]\n    fn test_only_child() {}\n    fn production_child() {}\n}\n";
    assert_eq!(
        analyze(source).unwrap(),
        FileMetrics {
            total_lines: 6,
            production_lines: 4,
            inline_test_lines: 2,
        }
    );
    // File-level constraints propagate the same way.
    let source = "#![cfg(any(test, feature = \"prod\"))]\n#[cfg(not(feature = \"prod\"))]\nfn test_only() {}\nfn production() {}\n";
    assert_eq!(analyze(source).unwrap().inline_test_lines, 2);
    // A child that widens nothing stays production under a production parent.
    let source = "#[cfg(feature = \"prod\")]\nmod prod {\n    #[cfg(unix)]\n    fn child() {}\n}\n";
    assert_eq!(analyze(source).unwrap().inline_test_lines, 0);
    // Constraints do not leak to siblings after leaving the parent.
    let source = "#[cfg(any(test, feature = \"prod\"))]\nmod mixed {\n    #[cfg(not(feature = \"prod\"))]\n    fn child() {}\n}\n#[cfg(not(feature = \"prod\"))]\nfn sibling() {}\n";
    assert_eq!(analyze(source).unwrap().inline_test_lines, 2);
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

// Issue #997: exempt-named files are classified from the module declaration
// graph. `run` indexes every file's declarations, propagates file-level
// gates through the graph, and then looks each exempt-named file up; these
// helpers replay that pipeline over a miniature repository.

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
fn declaration_forms_resolve_the_way_rust_loads_them() {
    assert_eq!(
        module_directory(Path::new("crates/x/src/foo.rs")),
        PathBuf::from("crates/x/src/foo")
    );
    assert_eq!(
        module_directory(Path::new("crates/x/src/foo/mod.rs")),
        PathBuf::from("crates/x/src/foo")
    );
    assert_eq!(
        module_directory(Path::new("crates/x/src/lib.rs")),
        PathBuf::from("crates/x/src")
    );
    // A cargo integration test target is a crate root, so it owns its
    // directory instead of a sibling named after its stem.
    assert_eq!(
        module_directory(Path::new("crates/x/tests/query.rs")),
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
