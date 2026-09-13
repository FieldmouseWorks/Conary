// crates/conary-xtask/src/line_cap/exemption/macros.rs

//! Opaque imports and transformation attributes require compiler authority.
//! The syntax graph records uncertainty instead of interpreting macro tokens.

/// External macro-use imports can replace prelude builtins and emit source that
/// is absent from this syntax graph. Keep them unresolved even before a call:
/// resolving their exported names and expansions requires compiler authority.
pub(super) fn reachable_external_import(
    attributes: &[syn::Attribute],
    inherited: &[syn::Attribute],
) -> bool {
    fn reachable(meta: &syn::Meta, conditions: &[syn::Attribute]) -> bool {
        if super::super::attributes::is_ident(meta.path(), "macro_use")
            || super::super::attributes::is_ident(meta.path(), "macro_escape")
        {
            return super::super::cfg::can_compile_without_test(conditions);
        }
        let syn::Meta::List(list) = meta else {
            return false;
        };
        if !super::super::attributes::is_ident(&list.path, "cfg_attr") {
            return false;
        }
        use syn::parse::Parser;
        let Ok(items) = syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated
            .parse2(list.tokens.clone())
        else {
            return true;
        };
        let mut items = items.into_iter();
        let Some(condition) = items.next() else {
            return true;
        };
        let mut nested = conditions.to_vec();
        nested.push(syn::parse_quote!(#[cfg(#condition)]));
        items.any(|meta| reachable(&meta, &nested))
    }
    attributes
        .iter()
        .any(|attribute| reachable(&attribute.meta, inherited))
}

/// Expand the ordered attribute list, preserving conditional test annotations
/// only after their declaring position. Rust Reference: attributes.meta.order-macro.
pub(super) fn attributes_require_expansion(
    attributes: &[syn::Attribute],
    inherited: &[syn::Attribute],
) -> bool {
    fn ordered(
        metas: impl IntoIterator<Item = syn::Meta>,
        conditions: &[syn::Attribute],
        mut preceding: Vec<syn::Attribute>,
    ) -> bool {
        for meta in metas {
            if !super::super::cfg::can_expand_without_test(conditions, &preceding) {
                return false;
            }
            let path = meta.path();
            if let syn::Meta::List(list) = &meta
                && super::super::attributes::is_ident(path, "cfg_attr")
            {
                use syn::parse::Parser;
                let Ok(items) =
                    syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated
                        .parse2(list.tokens.clone())
                else {
                    return true;
                };
                let mut items = items.into_iter();
                let Some(condition) = items.next() else {
                    return true;
                };
                let mut nested = conditions.to_vec();
                nested.push(syn::parse_quote!(#[cfg(#condition)]));
                if ordered(items, &nested, preceding.clone()) {
                    return true;
                }
            } else if super::builtin_attributes::parse(&meta).is_some()
                || super::super::attributes::is_ident(path, "cfg")
                || super::super::attributes::is_ident(path, "test")
                || super::super::attributes::is_ident(path, "macro_use")
                || super::super::attributes::is_ident(path, "macro_escape")
            {
                // Known attributes do not transform source here. A test marker
                // participates in reachability only for following attributes.
            } else if super::super::attributes::is_ident(path, "path") {
                if !matches!(&meta, syn::Meta::NameValue(value) if matches!(&value.value, syn::Expr::Lit(literal) if matches!(literal.lit, syn::Lit::Str(_))))
                {
                    return true;
                }
            } else {
                return true;
            }
            preceding.push(syn::parse_quote!(#[#meta]));
        }
        false
    }
    let mut configuration = inherited.to_vec();
    configuration.extend_from_slice(attributes);
    ordered(
        attributes.iter().map(|attribute| attribute.meta.clone()),
        &configuration,
        inherited.to_vec(),
    )
}
