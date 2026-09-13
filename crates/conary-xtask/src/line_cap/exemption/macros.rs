// crates/conary-xtask/src/line_cap/exemption/macros.rs

//! Local macro definitions are opaque source authority when their transcribers
//! can introduce outlined modules/includes, or when callers supply such tokens.
//! This discovers unresolved expansion; it does not certify a macro expansion.

use proc_macro2::{TokenStream, TokenTree};
use std::collections::{BTreeMap, BTreeSet};
use syn::ext::IdentExt;
use syn::visit::{self, Visit};

#[derive(Default)]
pub(super) struct MacroAuthority {
    definitions: BTreeMap<String, Vec<TokenStream>>,
    aliases: Vec<(String, String)>,
    local: BTreeSet<String>,
    source: BTreeSet<String>,
}

impl MacroAuthority {
    pub(super) fn collect<'a>(sources: impl IntoIterator<Item = &'a syn::File>) -> Self {
        let mut authority = Self::default();
        for syntax in sources {
            authority.visit_file(syntax);
        }
        authority
            .local
            .extend(authority.definitions.keys().cloned());
        loop {
            let before = (authority.local.len(), authority.source.len());
            for (name, bodies) in &authority.definitions {
                if bodies
                    .iter()
                    .any(|body| source_tokens(body.clone(), &authority.source))
                {
                    authority.source.insert(name.clone());
                }
            }
            for (original, alias) in &authority.aliases {
                if authority.local.contains(original) {
                    authority.local.insert(alias.clone());
                }
                if authority.source.contains(original) {
                    authority.source.insert(alias.clone());
                }
            }
            if before == (authority.local.len(), authority.source.len()) {
                break;
            }
        }
        authority
    }

    pub(super) fn requires_expansion(&self, mac: &syn::Macro) -> bool {
        let Some(name) = mac
            .path
            .segments
            .last()
            .map(|segment| segment.ident.unraw().to_string())
        else {
            return false;
        };
        self.source.contains(&name)
            || source_tokens(mac.tokens.clone(), &self.source)
            || (self.local.contains(&name)
                && references_source_macro(mac.tokens.clone(), &self.source))
    }

    fn aliases(&mut self, tree: &syn::UseTree) {
        match tree {
            syn::UseTree::Path(path) => self.aliases(&path.tree),
            syn::UseTree::Group(group) => {
                for item in &group.items {
                    self.aliases(item);
                }
            }
            syn::UseTree::Rename(rename) => self.aliases.push((
                rename.ident.unraw().to_string(),
                rename.rename.unraw().to_string(),
            )),
            syn::UseTree::Name(_) | syn::UseTree::Glob(_) => {}
        }
    }
}

impl<'ast> Visit<'ast> for MacroAuthority {
    fn visit_item_macro(&mut self, node: &'ast syn::ItemMacro) {
        if super::super::attributes::is_ident(&node.mac.path, "macro_rules")
            && let Some(name) = &node.ident
        {
            self.definitions
                .entry(name.unraw().to_string())
                .or_default()
                .push(node.mac.tokens.clone());
        }
        visit::visit_item_macro(self, node);
    }

    fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
        self.aliases(&node.tree);
    }
}

fn punct(token: Option<&TokenTree>, expected: char) -> bool {
    matches!(token, Some(TokenTree::Punct(punct)) if punct.as_char() == expected)
}

fn source_tokens(tokens: TokenStream, source_macros: &BTreeSet<String>) -> bool {
    let tokens: Vec<_> = tokens.into_iter().collect();
    for (index, token) in tokens.iter().enumerate() {
        match token {
            TokenTree::Ident(ident) => {
                let name = ident.unraw().to_string();
                if (name == "include" || source_macros.contains(&name))
                    && punct(tokens.get(index + 1), '!')
                {
                    return true;
                }
                if ident == "mod"
                    && !index
                        .checked_sub(1)
                        .is_some_and(|previous| punct(tokens.get(previous), '$'))
                {
                    // An inline module is visible syntax within this source.
                    // An outlined or token-dependent body needs expansion before
                    // the graph can establish which other source file it loads.
                    let body = match tokens.get(index + 1) {
                        Some(TokenTree::Ident(_)) => index + 2,
                        Some(TokenTree::Punct(dollar)) if dollar.as_char() == '$' => index + 3,
                        _ => return true,
                    };
                    if !matches!(tokens.get(body), Some(TokenTree::Group(group)) if group.delimiter() == proc_macro2::Delimiter::Brace)
                    {
                        return true;
                    }
                }
            }
            TokenTree::Group(group) => {
                // Nested opaque macro inputs can themselves become source. Do
                // not assume a particular expansion from a callee's spelling.
                if source_tokens(group.stream(), source_macros) {
                    return true;
                }
            }
            TokenTree::Punct(_) | TokenTree::Literal(_) => {}
        }
    }
    false
}

fn references_source_macro(tokens: TokenStream, source_macros: &BTreeSet<String>) -> bool {
    tokens.into_iter().any(|token| match token {
        TokenTree::Ident(ident) => source_macros.contains(&ident.unraw().to_string()),
        TokenTree::Group(group) => references_source_macro(group.stream(), source_macros),
        TokenTree::Punct(_) | TokenTree::Literal(_) => false,
    })
}
