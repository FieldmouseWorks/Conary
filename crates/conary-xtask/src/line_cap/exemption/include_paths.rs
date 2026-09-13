// crates/conary-xtask/src/line_cap/exemption/include_paths.rs

//! Literal include paths follow Rust 1.98.0 commit 88d9e12ae178fab0fb5cc050a94da85685d449ea:
//! compiler/rustc_builtin_macros/src/concat.rs and rustc_ast/src/util/literal.rs.
//! Integers render in decimal; floats retain their token spelling without
//! separators or suffixes. Bytes and C strings are not concat string inputs.

use syn::{Expr, Lit};

pub(super) fn include_path(expression: Expr) -> Option<String> {
    match expression {
        Expr::Lit(literal) => match literal.lit {
            Lit::Str(path) => Some(path.value()),
            _ => None,
        },
        Expr::Paren(parenthesized) => include_path(*parenthesized.expr),
        Expr::Macro(expression) if super::builtin_macro_path(&expression.mac.path, "concat") => {
            use syn::parse::Parser;
            let parser = syn::punctuated::Punctuated::<Expr, syn::Token![,]>::parse_terminated;
            parser
                .parse2(expression.mac.tokens)
                .ok()?
                .into_iter()
                .map(concat_value)
                .collect::<Option<String>>()
        }
        _ => None,
    }
}

fn concat_value(expression: Expr) -> Option<String> {
    match expression {
        Expr::Lit(literal) => match literal.lit {
            Lit::Str(value) => Some(value.value()),
            Lit::Char(value) => Some(value.value().to_string()),
            Lit::Bool(value) => Some(value.value.to_string()),
            numeric @ (Lit::Int(_) | Lit::Float(_)) => numeric_value(numeric),
            _ => None,
        },
        Expr::Unary(unary) if matches!(unary.op, syn::UnOp::Neg(_)) => {
            let Expr::Lit(literal) = *unary.expr else {
                return None;
            };
            Some(format!("-{}", numeric_value(literal.lit)?))
        }
        nested => include_path(nested),
    }
}

fn numeric_value(literal: Lit) -> Option<String> {
    match literal {
        Lit::Int(value) => Some(value.base10_parse::<u128>().ok()?.to_string()),
        Lit::Float(value) => {
            // syn's base10_digits normalizes E and removes exponent '+'. Rust
            // concat preserves both, so use the parsed literal's original token.
            let token = value.token().to_string();
            Some(token.strip_suffix(value.suffix())?.replace('_', ""))
        }
        _ => None,
    }
}
