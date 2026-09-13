// crates/conary-xtask/src/line_cap/tests/source_authority.rs

#![cfg(test)]

use super::*;

#[test]
fn include_aliases_fail_as_unresolved_source_authority() {
    for source in [
        r#"use std::include as inc; #[cfg(not(test))] inc!("tests.rs"); #[cfg(test)] mod tests;"#,
        r#"use core::{include as inc}; inc!("tests.rs");"#,
        r#"pub use std::include as public_include;"#,
        r#"fn run() { use std::include as inc; inc!("tests.rs"); }"#,
        r#"use std as rust; use rust::include as inc; inc!("tests.rs");"#,
    ] {
        let syntax = syn::parse_file(source).unwrap();
        let error = collect_include_declarations(
            &syntax,
            Path::new("crates/x/src/lib.rs"),
            &mut BTreeMap::new(),
        )
        .unwrap_err();
        assert!(
            error.contains("aliased include! imports require macro name resolution"),
            "{source}: {error}"
        );
    }
    for source in [
        "#[cfg(any())] use std::include as inc;",
        "use std::include;",
        "use std::include as include;",
        "use std::include as _;",
    ] {
        let syntax = syn::parse_file(source).unwrap();
        collect_include_declarations(
            &syntax,
            Path::new("crates/x/src/lib.rs"),
            &mut BTreeMap::new(),
        )
        .unwrap();
    }
}

#[test]
fn shadowed_builtin_macro_names_fail_as_unresolved_authority() {
    for source in [
        r#"macro_rules! include { ($path:literal) => { std::include!("tests.rs"); } } include!("ignored.rs"); #[cfg(test)] mod tests;"#,
        r#"macro_rules! concat { ($($tokens:tt)*) => { "tests.rs" } } include!(concat!("ignored.rs"));"#,
        "use custom::include;",
        "use custom::{concat};",
        "use custom::other as include;",
        "use custom::include as include;",
    ] {
        let syntax = syn::parse_file(source).unwrap();
        let error = collect_include_declarations(
            &syntax,
            Path::new("crates/x/src/lib.rs"),
            &mut BTreeMap::new(),
        )
        .unwrap_err();
        assert!(
            error.contains("local bindings shadow builtin include!/concat! macro authority"),
            "{source}: {error}"
        );
    }
    for source in [
        r#"#[cfg(any())] macro_rules! include { () => {}; }"#,
        "#[cfg(any())] use custom::include;",
        "use std::{include, concat};",
        "use core::concat as concat;",
    ] {
        let syntax = syn::parse_file(source).unwrap();
        collect_include_declarations(
            &syntax,
            Path::new("crates/x/src/lib.rs"),
            &mut BTreeMap::new(),
        )
        .unwrap();
    }
}

#[test]
fn raw_identifiers_preserve_module_and_macro_authority() {
    for load in [
        "mod r#tests;",
        r#"std::r#include!("tests.rs");"#,
        r#"r#std::r#include!(r#std::r#concat!("tests", ".rs"));"#,
        r#"#[r#path = "tests.rs"] mod production;"#,
        r#"#[r#cfg_attr(feature = "alternate", r#path = "tests.rs")] mod production;"#,
    ] {
        let source = format!("#[cfg(test)] #[path = \"tests.rs\"] mod tests_only;\n{load}");
        let classified = classify(&[
            ("crates/x/src/lib.rs", &source),
            ("crates/x/src/tests.rs", "pub fn helper() {}\n"),
        ]);
        assert_eq!(
            gate(&classified, "crates/x/src/tests.rs"),
            ExemptionGate::Ungated,
            "{load}"
        );
    }
    for source in [
        "use std::r#include as inc;",
        "use custom::other as r#include;",
        r#"macro_rules! r#include { () => {}; }"#,
        r#"macro_rules! r#concat { () => {}; }"#,
    ] {
        let syntax = syn::parse_file(source).unwrap();
        assert!(
            collect_include_declarations(
                &syntax,
                Path::new("crates/x/src/lib.rs"),
                &mut BTreeMap::new()
            )
            .is_err(),
            "{source}"
        );
    }
}

#[test]
fn raw_cfg_identifiers_have_the_same_predicate_identity() {
    for source in [
        "#[r#cfg(r#test)] fn helper() {}\n",
        "#[r#cfg_attr(r#all(), r#cfg(r#test))] fn helper() {}\n",
        "#[cfg(not(feature = \"x\"))] #[cfg(any(test, r#feature = \"x\"))] fn helper() {}\n",
        "#[r#test] fn helper() {}\n",
    ] {
        assert_eq!(
            analyze_source(source).unwrap().production_lines,
            0,
            "{source}"
        );
    }
}

#[test]
fn local_source_generating_macros_fail_as_unresolved_authority() {
    for source in [
        r#"macro_rules! load { () => { mod tests; } } #[cfg(not(test))] load!(); #[cfg(test)] mod tests;"#,
        r#"macro_rules! load { () => { include!("tests.rs"); } } load!();"#,
        r#"macro_rules! load { ($name:ident) => { mod $name; } } load!(tests);"#,
        r#"macro_rules! inline { () => { mod tests {} } } inline!();"#,
        r#"fn run() { macro_rules! ordinary { () => { let r#mod = (); }; } ordinary!(); }"#,
        r#"macro_rules! forward { ($item:item) => { $item } } forward!(mod tests;);"#,
        r#"macro_rules! load { () => { mod tests; } } use load as alias; alias!();"#,
        r#"macro_rules! load { () => { mod tests; } } macro_rules! outer { () => { load!(); } } outer!();"#,
        r#"macro_rules! ast { () => { syn::parse_quote!(mod tests;) } } ast!();"#,
        r#"macro_rules! load { () => { mod tests; } } macro_rules! call { ($callback:ident) => { $callback!(); } } call!(load);"#,
    ] {
        let syntax = syn::parse_file(source).unwrap();
        let error = collect_include_declarations(
            &syntax,
            Path::new("crates/x/src/lib.rs"),
            &mut BTreeMap::new(),
        )
        .unwrap_err();
        assert!(
            error.contains("macro expansion may introduce source declarations"),
            "{source}: {error}"
        );
    }
    for source in [
        r#"macro_rules! unused { () => { mod tests; } }"#,
        r#"macro_rules! load { () => { mod tests; } } #[cfg(any())] load!();"#,
    ] {
        let syntax = syn::parse_file(source).unwrap();
        collect_include_declarations(
            &syntax,
            Path::new("crates/x/src/lib.rs"),
            &mut BTreeMap::new(),
        )
        .unwrap();
    }
}

#[test]
fn include_arguments_accept_one_expression_and_an_optional_comma() {
    for load in [
        r#"include!("tests.rs",);"#,
        r#"std::include!(concat!("tests", ".rs"),);"#,
    ] {
        let source = format!("#[cfg(test)] mod tests;\n{load}");
        let classified = classify(&[
            ("crates/x/src/lib.rs", &source),
            ("crates/x/src/tests.rs", "fn helper() {}\n"),
        ]);
        assert_eq!(
            gate(&classified, "crates/x/src/tests.rs"),
            ExemptionGate::Ungated
        );
    }
    for source in ["include!();", r#"include!("tests.rs", "extra");"#] {
        let syntax = syn::parse_file(source).unwrap();
        assert!(
            collect_include_declarations(
                &syntax,
                Path::new("crates/x/src/lib.rs"),
                &mut BTreeMap::new()
            )
            .is_err()
        );
    }
}

#[test]
fn source_macro_exports_and_aliases_are_checked_across_files() {
    let sources = BTreeMap::from([
        ("crates/x/src/lib.rs".to_string(), syn::parse_file(r#"mod macros; use crate::load as renamed; #[cfg(not(test))] renamed!(); #[cfg(test)] mod tests;"#).unwrap()),
        ("crates/x/src/macros.rs".to_string(), syn::parse_file(r#"#[macro_export] macro_rules! load { () => { mod tests; } }"#).unwrap()),
        ("crates/x/src/tests.rs".to_string(), syn::parse_file("fn helper() {}").unwrap()),
    ]);
    let result = collect_source_graph(
        &sources,
        &fixture_targets(sources.keys().map(String::as_str)),
    );
    let graph = result.unwrap();
    assert!(!graph.unresolved_sources.is_empty());
    assert_eq!(graph.gates["crates/x/src/tests.rs"], ExemptionGate::Unknown);
}

#[test]
fn opaque_macro_source_arguments_require_expansion() {
    for source in [
        "external::emit!(mod tests;);",
        r#"fn run() { std::assert!({ #[path = "tests.rs"] mod tests; true }); }"#,
        "syn::parse_quote!(mod tests;);",
    ] {
        let syntax = syn::parse_file(source).unwrap();
        let error = collect_include_declarations(
            &syntax,
            Path::new("crates/x/src/lib.rs"),
            &mut BTreeMap::new(),
        )
        .unwrap_err();
        assert!(
            error.contains("macro expansion may introduce source declarations"),
            "{source}: {error}"
        );
    }
}

#[test]
fn external_macro_use_imports_require_name_resolution_and_expansion() {
    for source in [
        r#"#[macro_use] extern crate dep; #[cfg(not(test))] include!("ignored.rs"); #[cfg(test)] mod tests;"#,
        r#"#[macro_use(concat)] extern crate dep; std::include!(concat!("ignored.rs"));"#,
        r#"#[r#macro_use(r#include)] extern crate dep as renamed; r#include!("ignored.rs");"#,
        "#[cfg_attr(feature = \"external\", macro_use)] extern crate dep;",
        "#[cfg_attr(not(test), cfg_attr(feature = \"external\", macro_use(include)))] extern crate dep;",
        "#[macro_use] extern crate dep;",
    ] {
        let syntax = syn::parse_file(source).unwrap();
        let error = collect_include_declarations(
            &syntax,
            Path::new("crates/x/src/lib.rs"),
            &mut BTreeMap::new(),
        )
        .unwrap_err();
        assert!(
            error.contains(
                "macro_use extern crate imports require macro name resolution and expansion"
            ),
            "{source}: {error}"
        );
    }
}

#[test]
fn impossible_macro_use_imports_do_not_block_builtin_includes() {
    for import in [
        "extern crate dep;",
        "#[cfg(any())] #[macro_use] extern crate dep;",
        "#[cfg_attr(any(), macro_use)] extern crate dep;",
        "#[cfg(not(test))] #[cfg_attr(test, macro_use)] extern crate dep;",
        "#[cfg_attr(test, cfg_attr(not(test), macro_use))] extern crate dep;",
        "#[cfg(any())] mod dead { #[macro_use] extern crate dep; }",
    ] {
        let syntax = syn::parse_file(&format!("{import} include!(\"tests.rs\");")).unwrap();
        let mut declarations = BTreeMap::new();
        collect_include_declarations(&syntax, Path::new("crates/x/src/lib.rs"), &mut declarations)
            .unwrap_or_else(|error| panic!("{import}: {error}"));
        assert_eq!(declarations["crates/x/src/tests.rs"].len(), 1, "{import}");
    }
}

#[test]
fn external_macro_use_cannot_certify_a_test_file_across_sources() {
    let sources = BTreeMap::from([
        (
            "crates/x/src/lib.rs".to_string(),
            syn::parse_file("#[macro_use] extern crate dep; mod implementation;").unwrap(),
        ),
        (
            "crates/x/src/implementation.rs".to_string(),
            syn::parse_file(r#"#[cfg(not(test))] include!("ignored.rs"); #[cfg(test)] mod tests;"#)
                .unwrap(),
        ),
        (
            "crates/x/src/implementation/tests.rs".to_string(),
            syn::parse_file("fn helper() {}").unwrap(),
        ),
    ]);
    let result = collect_source_graph(
        &sources,
        &fixture_targets(sources.keys().map(String::as_str)),
    );
    let graph = result.unwrap();
    assert!(!graph.unresolved_sources.is_empty());
    assert_eq!(
        graph.gates["crates/x/src/implementation/tests.rs"],
        ExemptionGate::Unknown
    );
}

#[test]
fn opaque_expansions_cannot_certify_contextual_test_exemptions() {
    for source in [
        "use dep::emit; #[cfg(not(test))] emit!();",
        "#[cfg(not(test))] dep::emit!();",
        "use dep::emit as renamed; #[cfg(not(test))] renamed!();",
        "#[dep::emit] struct Input;",
        "#[derive(dep::Emit)] struct Input;",
        "#[cfg_attr(not(test), dep::emit)] struct Input;",
        "#[dep::test] fn input() {}",
        "use dep::*;",
        r#"use dep as std; #[cfg(not(test))] std::include!("ignored.rs");"#,
    ] {
        for intrinsic in [false, true] {
            let mut sources = BTreeMap::from([
                (
                    "crates/x/src/lib.rs".to_owned(),
                    syn::parse_file(&format!("{source} #[cfg(test)] mod tests;")).unwrap(),
                ),
                (
                    "crates/x/src/tests.rs".to_owned(),
                    syn::parse_file("fn helper() {} ").unwrap(),
                ),
            ]);
            if intrinsic {
                sources
                    .get_mut("crates/x/src/tests.rs")
                    .unwrap()
                    .attrs
                    .push(syn::parse_quote!(#![cfg(test)]));
            }
            let graph = collect_source_graph(
                &sources,
                &fixture_targets(sources.keys().map(String::as_str)),
            )
            .unwrap();
            assert!(!graph.unresolved_sources.is_empty(), "{source}");
            assert_eq!(
                graph.gates["crates/x/src/tests.rs"],
                if intrinsic {
                    ExemptionGate::TestGated
                } else {
                    ExemptionGate::Unknown
                },
                "{source} (intrinsic={intrinsic})"
            );
        }
    }
}

#[test]
fn test_only_expansions_do_not_introduce_production_uncertainty() {
    for source in [
        "#[cfg(test)] dep::emit!();",
        "#[test] fn example() { dep::emit!(); }",
        "#[cfg_attr(all(), test)] fn example() { dep::emit!(); }",
        "#[cfg_attr(test, dep::emit)] struct Input;",
        "#[cfg_attr(test, macro_use)] extern crate dep;",
        "#[cfg(any())] dep::emit!();",
    ] {
        let syntax = syn::parse_file(source).unwrap();
        collect_include_declarations(
            &syntax,
            Path::new("crates/x/src/lib.rs"),
            &mut BTreeMap::new(),
        )
        .unwrap_or_else(|error| panic!("{source}: {error}"));
    }
}

#[test]
fn apparent_builtin_calls_cannot_certify_the_extern_prelude() {
    for call in [
        r#"include!("ignored.rs");"#,
        r#"std::include!("ignored.rs");"#,
        r#"core::include!("ignored.rs");"#,
    ] {
        let sources = BTreeMap::from([
            (
                "crates/x/src/lib.rs".to_owned(),
                syn::parse_file(&format!("{call} #[cfg(test)] mod tests;")).unwrap(),
            ),
            (
                "crates/x/src/tests.rs".to_owned(),
                syn::parse_file("fn helper() {}").unwrap(),
            ),
        ]);
        let graph = collect_source_graph(
            &sources,
            &fixture_targets(sources.keys().map(String::as_str)),
        )
        .unwrap();
        assert!(!graph.unresolved_sources.is_empty(), "{call}");
        assert_eq!(
            graph.gates["crates/x/src/tests.rs"],
            ExemptionGate::Unknown,
            "{call}"
        );
    }
}

#[test]
fn test_context_expansions_cannot_seed_a_production_load() {
    for production in [false, true] {
        let sources = BTreeMap::from([
            (
                "crates/x/src/lib.rs".to_owned(),
                syn::parse_file(if production {
                    "dep::emit!(); #[cfg(test)] mod helper;"
                } else {
                    "#[cfg(test)] mod helper;"
                })
                .unwrap(),
            ),
            (
                "crates/x/src/helper.rs".to_owned(),
                syn::parse_file("dep::emit!(); mod tests;").unwrap(),
            ),
            (
                "crates/x/src/helper/tests.rs".to_owned(),
                syn::parse_file("fn helper() {}").unwrap(),
            ),
        ]);
        let graph = collect_source_graph(
            &sources,
            &fixture_targets(sources.keys().map(String::as_str)),
        )
        .unwrap();
        assert_eq!(graph.unresolved_sources.is_empty(), !production);
        assert_eq!(
            graph.gates["crates/x/src/helper/tests.rs"],
            if production {
                ExemptionGate::Unknown
            } else {
                ExemptionGate::TestGated
            }
        );
    }
}

#[test]
fn unscanned_exact_loads_invalidate_context_only_exemptions() {
    for intrinsic in [false, true] {
        let sources = BTreeMap::from([
            ("crates/x/src/lib.rs".to_owned(), syn::parse_file(r#"#[path = "../../../third_party/bridge.rs"] mod bridge; #[cfg(test)] mod tests;"#).unwrap()),
            ("crates/x/src/tests.rs".to_owned(), syn::parse_file(if intrinsic { "#![cfg(test)] fn helper() {}" } else { "fn helper() {}" }).unwrap()),
        ]);
        let graph = collect_source_graph(
            &sources,
            &fixture_targets(sources.keys().map(String::as_str)),
        )
        .unwrap();
        assert!(
            graph.unresolved_sources["crates/x/src/lib.rs"].contains("unscanned Rust load target")
        );
        assert_eq!(
            graph.gates["crates/x/src/tests.rs"],
            if intrinsic {
                ExemptionGate::TestGated
            } else {
                ExemptionGate::Unknown
            }
        );
    }
}

#[test]
fn absent_conventional_alternatives_do_not_make_a_complete_graph_unknown() {
    for implementation in [
        "crates/x/src/implementation.rs",
        "crates/x/src/implementation/mod.rs",
    ] {
        let sources = BTreeMap::from([
            (
                "crates/x/src/lib.rs".to_owned(),
                syn::parse_file("mod implementation; #[cfg(test)] mod tests;").unwrap(),
            ),
            (
                implementation.to_owned(),
                syn::parse_file("fn helper() {}").unwrap(),
            ),
            (
                "crates/x/src/tests.rs".to_owned(),
                syn::parse_file("fn helper() {}").unwrap(),
            ),
        ]);
        let graph = collect_source_graph(
            &sources,
            &fixture_targets(sources.keys().map(String::as_str)),
        )
        .unwrap();
        assert!(graph.unresolved_sources.is_empty(), "{implementation}");
        assert_eq!(
            graph.gates["crates/x/src/tests.rs"],
            ExemptionGate::TestGated
        );
    }
}

#[test]
fn missing_production_module_candidates_keep_contextual_exemptions_unknown() {
    let sources = BTreeMap::from([
        (
            "crates/x/src/lib.rs".to_owned(),
            syn::parse_file("mod missing; #[cfg(test)] mod tests;").unwrap(),
        ),
        (
            "crates/x/src/tests.rs".to_owned(),
            syn::parse_file("fn helper() {}").unwrap(),
        ),
    ]);
    let graph = collect_source_graph(
        &sources,
        &fixture_targets(sources.keys().map(String::as_str)),
    )
    .unwrap();
    assert!(!graph.unresolved_sources.is_empty());
    assert_eq!(graph.gates["crates/x/src/tests.rs"], ExemptionGate::Unknown);
}

#[test]
fn unscanned_test_only_loads_cannot_seed_production_uncertainty() {
    for root in [
        r#"#[cfg(test)] #[path = "../../../third_party/bridge.rs"] mod bridge; #[cfg(test)] mod tests;"#,
        "#[cfg(test)] mod helper; #[cfg(test)] mod tests;",
    ] {
        let sources = BTreeMap::from([
            (
                "crates/x/src/lib.rs".to_owned(),
                syn::parse_file(root).unwrap(),
            ),
            (
                "crates/x/src/helper.rs".to_owned(),
                syn::parse_file(r#"#[path = "../../../third_party/bridge.rs"] mod bridge;"#)
                    .unwrap(),
            ),
            (
                "crates/x/src/tests.rs".to_owned(),
                syn::parse_file("fn helper() {}").unwrap(),
            ),
        ]);
        // The first case has no helper source in its graph; do not introduce an
        // unrelated orphan that might be another production entry point.
        let mut sources = sources;
        if !root.contains("mod helper;") {
            sources.remove("crates/x/src/helper.rs");
        }
        let graph = collect_source_graph(
            &sources,
            &fixture_targets(sources.keys().map(String::as_str)),
        )
        .unwrap();
        assert!(graph.unresolved_sources.is_empty(), "{root}");
        assert_eq!(
            graph.gates["crates/x/src/tests.rs"],
            ExemptionGate::TestGated
        );
    }
}

#[test]
fn compiler_owned_inert_attributes_preserve_complete_contextual_proof() {
    for prefix in [
        "#![allow(dead_code)] //! Documentation\n",
        "#[repr(C)] struct Input { value: u8 }",
        "#[r#repr(C)] struct Input { value: u8 }",
        "#[inline] #[must_use] fn helper() -> u8 { 0 }",
        "#[unsafe(no_mangle)] pub extern \"C\" fn helper() {}",
        "#[doc = \"metadata\"] struct Input;",
        "#[doc = concat!(\"meta\", \"data\")] struct Input;",
        "#[cfg_attr(all(), doc = concat!(\"meta\", \"data\"))] struct Input;",
        "#![no_std]",
        "#![no_implicit_prelude]",
    ] {
        let sources = BTreeMap::from([
            (
                "crates/x/src/lib.rs".to_owned(),
                syn::parse_file(&format!("{prefix}\n#[cfg(test)] mod tests;")).unwrap(),
            ),
            (
                "crates/x/src/tests.rs".to_owned(),
                syn::parse_file("fn helper() {}").unwrap(),
            ),
        ]);
        let graph = collect_source_graph(
            &sources,
            &fixture_targets(sources.keys().map(String::as_str)),
        )
        .unwrap();
        assert!(graph.unresolved_sources.is_empty(), "{prefix}");
        assert_eq!(
            graph.gates["crates/x/src/tests.rs"],
            ExemptionGate::TestGated,
            "{prefix}"
        );
    }
}

#[test]
fn registered_expansions_and_qualified_attribute_names_stay_unresolved() {
    for attribute in [
        "#[derive(Clone)]",
        "#[dep::allow]",
        "#[dep::doc]",
        "#[global_allocator]",
    ] {
        let syntax = syn::parse_file(&format!("{attribute} struct Input;")).unwrap();
        assert!(
            collect_include_declarations(
                &syntax,
                Path::new("crates/x/src/lib.rs"),
                &mut BTreeMap::new()
            )
            .is_err(),
            "{attribute}"
        );
    }
}

#[test]
fn opaque_path_attribute_values_do_not_certify_a_literal_fallback() {
    let sources = BTreeMap::from([
        ("crates/x/src/lib.rs".to_owned(), syn::parse_file(r#"#[cfg_attr(not(test), path = env!("SOURCE"))] mod implementation; #[cfg(test)] mod tests;"#).unwrap()),
        ("crates/x/src/implementation.rs".to_owned(), syn::parse_file("fn helper() {}").unwrap()),
        ("crates/x/src/tests.rs".to_owned(), syn::parse_file("fn helper() {}").unwrap()),
    ]);
    let graph = collect_source_graph(
        &sources,
        &fixture_targets(sources.keys().map(String::as_str)),
    )
    .unwrap();
    assert!(!graph.unresolved_sources.is_empty());
    assert_eq!(graph.gates["crates/x/src/tests.rs"], ExemptionGate::Unknown);
}

#[test]
fn unscanned_cargo_targets_retain_production_uncertainty() {
    for file in ["third_party/root.rs", "/external/root.rs"] {
        for kind in [CargoTarget::Other, CargoTarget::Test] {
            let sources = BTreeMap::from([
                (
                    "crates/x/src/lib.rs".to_owned(),
                    syn::parse_file("#[cfg(test)] mod tests;").unwrap(),
                ),
                (
                    "crates/x/src/tests.rs".to_owned(),
                    syn::parse_file("fn helper() {}").unwrap(),
                ),
            ]);
            let mut targets = fixture_targets(sources.keys().map(String::as_str));
            targets.insert(file.to_owned(), kind);
            let graph = collect_source_graph(&sources, &targets).unwrap();
            assert_eq!(
                graph.unresolved_sources.is_empty(),
                kind == CargoTarget::Test
            );
            assert_eq!(
                graph.gates["crates/x/src/tests.rs"],
                if kind == CargoTarget::Test {
                    ExemptionGate::TestGated
                } else {
                    ExemptionGate::Unknown
                }
            );
        }
    }
}
