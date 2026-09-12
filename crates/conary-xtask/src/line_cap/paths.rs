// crates/conary-xtask/src/line_cap/paths.rs

//! Preserve the conditions selecting module source paths. The default path
//! exists only where no path attribute applies; nested cfg_attr conditions
//! conjoin rather than turning a conditional import into an unconditional one.

use syn::{Attribute, Expr, Lit, Meta, Token, parse::Parser, punctuated::Punctuated};

pub(super) struct PathVariant {
    pub(super) path: Option<String>,
    pub(super) conditions: Vec<Attribute>,
}

pub(super) fn module_paths(attributes: &[Attribute]) -> Vec<PathVariant> {
    fn collect(meta: &Meta, predicates: &[Meta], paths: &mut Vec<(String, Vec<Meta>)>) {
        match meta {
            Meta::NameValue(named) if named.path.is_ident("path") => {
                if let Expr::Lit(literal) = &named.value
                    && let Lit::Str(path) = &literal.lit
                {
                    paths.push((path.value(), predicates.to_vec()));
                }
            }
            Meta::List(list) if list.path.is_ident("cfg_attr") => {
                if let Ok(items) =
                    Punctuated::<Meta, Token![,]>::parse_terminated.parse2(list.tokens.clone())
                {
                    let mut items = items.into_iter();
                    if let Some(condition) = items.next() {
                        let mut nested = predicates.to_vec();
                        nested.push(condition);
                        for meta in items {
                            collect(&meta, &nested, paths);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    let mut paths = Vec::new();
    for attribute in attributes {
        collect(&attribute.meta, &[], &mut paths);
    }
    let selected: Vec<Meta> = paths
        .iter()
        .map(|(_, predicates)| syn::parse_quote!(all(#(#predicates),*)))
        .collect();
    let fallback: Attribute = syn::parse_quote!(#[cfg(not(any(#(#selected),*)))]);
    let mut variants: Vec<_> = paths
        .into_iter()
        .map(|(path, predicates)| PathVariant {
            path: Some(path),
            conditions: predicates
                .into_iter()
                .map(|predicate| syn::parse_quote!(#[cfg(#predicate)]))
                .collect(),
        })
        .collect();
    variants.push(PathVariant {
        path: None,
        conditions: vec![fallback],
    });
    variants
}
