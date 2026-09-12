// crates/conary-xtask/src/line_cap/attributes.rs

//! Shared traversal of attributed syntax nodes for span and declaration analysis.
//! All visitors supply `descend` so enclosing conditions cover the same nodes.

// Each of these typed nodes owns outer attributes and an exact syntax span.
macro_rules! visit_nodes {
    ($($method:ident: $node:ident),* $(,)?) => {$ (
        fn $method(&mut self, node: &'ast syn::$node) {
            self.descend(&node.attrs, node.span(), |visitor| visit::$method(visitor, node));
        }
    )*};
}

macro_rules! visit_attributed_nodes {
    () => {
        visit_nodes! {
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
    };
}

pub(super) use visit_attributed_nodes;
pub(super) use visit_nodes;

/// Raw identifiers and ordinary identifiers name the same Rust symbol.
pub(super) fn is_ident(path: &syn::Path, name: &str) -> bool {
    use syn::ext::IdentExt;
    path.get_ident().is_some_and(|ident| ident.unraw() == name)
}
