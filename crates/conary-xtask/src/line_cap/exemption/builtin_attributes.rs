// crates/conary-xtask/src/line_cap/exemption/builtin_attributes.rs

//! Compiler-owned inert names from Rust 1.98.0, commit
//! 88d9e12ae178fab0fb5cc050a94da85685d449ea, the stable BUILTIN_ATTRIBUTES
//! registry in compiler/rustc_feature/src/builtin_attrs.rs. cfg/cfg_attr/path
//! and macro imports have separate source semantics. derive/test belong to
//! register_attr! in compiler/rustc_builtin_macros/src/lib.rs, outside this
//! inert registry. Unknown names and qualified paths require macro resolution.

use syn::ext::IdentExt;

pub(super) enum InertBuiltin {
    Ignore,
    ShouldPanic,
    AutomaticallyDerived,
    MacroExport,
    ProcMacro,
    ProcMacroDerive,
    ProcMacroAttribute,
    Warn,
    Allow,
    Expect,
    Forbid,
    Deny,
    MustUse,
    MustNotSuspend,
    Deprecated,
    CrateName,
    CrateType,
    Link,
    LinkName,
    NoLink,
    Repr,
    RustcAlign,
    RustcAlignStatic,
    ExportName,
    LinkSection,
    NoMangle,
    Used,
    LinkOrdinal,
    Naked,
    RustcPassIndirectlyInNonRusticAbis,
    RecursionLimit,
    TypeLengthLimit,
    MoveSizeLimit,
    NoMain,
    NoStd,
    NoImplicitPrelude,
    NonExhaustive,
    WindowsSubsystem,
    PanicHandler,
    Inline,
    Cold,
    NoBuiltins,
    TargetFeature,
    TrackCaller,
    InstructionSet,
    ForceTargetFeature,
    Sanitize,
    Coverage,
    Doc,
    DebuggerVisualizer,
    CollapseDebuginfo,
}

pub(super) fn parse(meta: &syn::Meta) -> Option<InertBuiltin> {
    if super::super::attributes::is_ident(meta.path(), "unsafe") {
        let syn::Meta::List(list) = meta else {
            return None;
        };
        let nested = syn::parse2::<syn::Meta>(list.tokens.clone()).ok()?;
        return match parse(&nested)? {
            kind @ (InertBuiltin::ExportName
            | InertBuiltin::LinkSection
            | InertBuiltin::NoMangle
            | InertBuiltin::Naked) => Some(kind),
            _ => None,
        };
    }
    Some(
        match meta.path().get_ident()?.unraw().to_string().as_str() {
            "ignore" => InertBuiltin::Ignore,
            "should_panic" => InertBuiltin::ShouldPanic,
            "automatically_derived" => InertBuiltin::AutomaticallyDerived,
            "macro_export" => InertBuiltin::MacroExport,
            "proc_macro" => InertBuiltin::ProcMacro,
            "proc_macro_derive" => InertBuiltin::ProcMacroDerive,
            "proc_macro_attribute" => InertBuiltin::ProcMacroAttribute,
            "warn" => InertBuiltin::Warn,
            "allow" => InertBuiltin::Allow,
            "expect" => InertBuiltin::Expect,
            "forbid" => InertBuiltin::Forbid,
            "deny" => InertBuiltin::Deny,
            "must_use" => InertBuiltin::MustUse,
            "must_not_suspend" => InertBuiltin::MustNotSuspend,
            "deprecated" => InertBuiltin::Deprecated,
            "crate_name" => InertBuiltin::CrateName,
            "crate_type" => InertBuiltin::CrateType,
            "link" => InertBuiltin::Link,
            "link_name" => InertBuiltin::LinkName,
            "no_link" => InertBuiltin::NoLink,
            "repr" => InertBuiltin::Repr,
            "rustc_align" => InertBuiltin::RustcAlign,
            "rustc_align_static" => InertBuiltin::RustcAlignStatic,
            "export_name" => InertBuiltin::ExportName,
            "link_section" => InertBuiltin::LinkSection,
            "no_mangle" => InertBuiltin::NoMangle,
            "used" => InertBuiltin::Used,
            "link_ordinal" => InertBuiltin::LinkOrdinal,
            "naked" => InertBuiltin::Naked,
            "rustc_pass_indirectly_in_non_rustic_abis" => {
                InertBuiltin::RustcPassIndirectlyInNonRusticAbis
            }
            "recursion_limit" => InertBuiltin::RecursionLimit,
            "type_length_limit" => InertBuiltin::TypeLengthLimit,
            "move_size_limit" => InertBuiltin::MoveSizeLimit,
            "no_main" => InertBuiltin::NoMain,
            "no_std" => InertBuiltin::NoStd,
            "no_implicit_prelude" => InertBuiltin::NoImplicitPrelude,
            "non_exhaustive" => InertBuiltin::NonExhaustive,
            "windows_subsystem" => InertBuiltin::WindowsSubsystem,
            "panic_handler" => InertBuiltin::PanicHandler,
            "inline" => InertBuiltin::Inline,
            "cold" => InertBuiltin::Cold,
            "no_builtins" => InertBuiltin::NoBuiltins,
            "target_feature" => InertBuiltin::TargetFeature,
            "track_caller" => InertBuiltin::TrackCaller,
            "instruction_set" => InertBuiltin::InstructionSet,
            "force_target_feature" => InertBuiltin::ForceTargetFeature,
            "sanitize" => InertBuiltin::Sanitize,
            "coverage" => InertBuiltin::Coverage,
            "doc" => InertBuiltin::Doc,
            "debugger_visualizer" => InertBuiltin::DebuggerVisualizer,
            "collapse_debuginfo" => InertBuiltin::CollapseDebuginfo,
            _ => return None,
        },
    )
}
