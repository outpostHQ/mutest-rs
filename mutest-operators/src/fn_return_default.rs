use mutest_emit::{Mutation, Operator};
use mutest_emit::analysis::res;
use mutest_emit::analysis::ty;
use mutest_emit::codegen::ast;
use mutest_emit::codegen::mutation::{MutCtxt, MutLoc, Mutations, Subst, SubstDef, SubstLoc};
use mutest_emit::codegen::symbols::{Symbol, path, sym};
use rustc_data_structures::smallvec::{SmallVec, smallvec};
use rustc_data_structures::thin_vec::thin_vec;
use rustc_middle::ty::{Ty, TyCtxt, TyKind};
use rustc_span::Span;
use rustc_span::def_id::LocalDefId;

pub const FN_RETURN_DEFAULT: &str = "fn_return_default";

pub struct FnReturnDefaultMutation {
    pub value: String,
}

impl Mutation for FnReturnDefaultMutation {
    fn op_name(&self) -> &str { FN_RETURN_DEFAULT }

    fn display_name(&self) -> String {
        format!("return `{value}` without evaluating the function body", value = self.value)
    }

    fn span_label(&self) -> String {
        self.display_name()
    }
}

/// A value a function could return instead of computing one, with the source text to name it by.
struct Replacement {
    value: String,
    expr: Box<ast::Expr>,
}

impl Replacement {
    fn new(value: impl Into<String>, expr: Box<ast::Expr>) -> Self {
        Self { value: value.into(), expr }
    }
}

/// Nesting past this is where the count grows faster than the value it adds.
const MAX_TY_DEPTH: u32 = 3;

/// Values worth returning in place of `ty`. Every one must type-check: mutest compiles all
/// mutations into one binary, so an unviable one is a build failure, not a discarded mutant.
fn replacements<'tcx>(tcx: TyCtxt<'tcx>, body_def_id: LocalDefId, sp: Span, ty: Ty<'tcx>, depth: u32) -> Vec<Replacement> {
    let has_default = |t| ty::impls_trait(tcx, body_def_id, t, res::traits::Default(tcx), vec![]);
    let default_expr = || ast::mk::expr_call_path(sp, path::default(sp), thin_vec![]);
    let float = |v: &str| ast::mk::expr_lit(sp, ast::token::LitKind::Float, Symbol::intern(v), None);

    // What any type falls back to, and all a type is worth once the nesting is too deep to be
    // worth enumerating.
    let default_only = || match has_default(ty) {
        true => vec![Replacement::new("Default::default()", default_expr())],
        false => vec![],
    };

    if depth > MAX_TY_DEPTH { return default_only(); }

    // Wrap each of the inner type's values, so `Result<bool, E>` yields `Ok(true)` and `Ok(false)`
    // rather than only the inner default.
    let wrapped = |inner: Ty<'tcx>, ctor: ast::Path, name: &str| -> Vec<Replacement> {
        replacements(tcx, body_def_id, sp, inner, depth + 1).into_iter()
            .map(|r| Replacement::new(
                format!("{name}({value})", value = r.value),
                ast::mk::expr_call_path(sp, ctor.clone(), thin_vec![r.expr]),
            ))
            .collect()
    };

    match ty.kind() {
        // The default alone leaves a body that always returns the other value uncaught.
        TyKind::Bool => vec![
            Replacement::new("false", ast::mk::expr_bool(sp, false)),
            Replacement::new("true", ast::mk::expr_bool(sp, true)),
        ],

        TyKind::Int(_) => vec![
            Replacement::new("0", ast::mk::expr_int(sp, 0)),
            Replacement::new("1", ast::mk::expr_int(sp, 1)),
            Replacement::new("-1", ast::mk::expr_int(sp, -1)),
        ],
        // `-1` does not type-check unsigned, and an unviable mutation fails the whole build.
        TyKind::Uint(_) => vec![
            Replacement::new("0", ast::mk::expr_int(sp, 0)),
            Replacement::new("1", ast::mk::expr_int(sp, 1)),
        ],
        TyKind::Float(_) => vec![
            Replacement::new("0.0", float("0.0")),
            Replacement::new("1.0", float("1.0")),
            Replacement::new("-1.0", ast::mk::expr_unary(sp, ast::UnOp::Neg, float("1.0"))),
        ],

        TyKind::Tuple(elems) if elems.is_empty() => vec![Replacement::new("()", ast::mk::expr_tuple(sp, thin_vec![]))],

        // A `&'static str` satisfies any shorter lifetime the signature asks for.
        TyKind::Ref(_, inner, ast::Mutability::Not) if inner.is_str() => vec![
            Replacement::new(r#""""#, ast::mk::expr_str(sp, "")),
            Replacement::new(r#""xyzzy""#, ast::mk::expr_str(sp, "xyzzy")),
        ],

        TyKind::Adt(adt_def, generic_args) => {
            let did = adt_def.did();

            // `Result` has no `Default`, so without this arm every fallible function is skipped —
            // in a codebase that returns `Result` throughout, that is nearly all of them.
            if tcx.is_diagnostic_item(sym::Result, did) {
                return wrapped(generic_args.type_at(0), path::Ok(sp), "Ok");
            }

            if tcx.is_diagnostic_item(sym::Option, did) {
                let mut out = vec![Replacement::new("None", ast::mk::expr_path(path::None(sp)))];
                out.extend(wrapped(generic_args.type_at(0), path::Some(sp), "Some"));
                return out;
            }

            if tcx.is_diagnostic_item(sym::Vec, did) {
                let mut out = vec![Replacement::new("Vec::new()", default_expr())];
                out.extend(replacements(tcx, body_def_id, sp, generic_args.type_at(0), depth + 1).into_iter()
                    .map(|r| Replacement::new(
                        format!("Vec::from([{value}])", value = r.value),
                        ast::mk::expr_call_path(sp, path::vec_from(sp), thin_vec![ast::mk::expr_array(sp, thin_vec![r.expr])]),
                    )));
                return out;
            }

            // `String` carries no diagnostic item, unlike `Option` and `Result`; the lang item is
            // the only handle on it.
            if tcx.lang_items().string() == Some(did) {
                return vec![
                    Replacement::new("String::new()", default_expr()),
                    Replacement::new(r#"String::from("xyzzy")"#,
                        ast::mk::expr_call_path(sp, path::string_from(sp), thin_vec![ast::mk::expr_str(sp, "xyzzy")])),
                ];
            }

            default_only()
        }

        _ => default_only(),
    }
}

/// Return a fixed value from a function without running its body, to check that some test depends
/// on what the function computes rather than on it merely being called.
pub struct FnReturnDefault;

impl<'a> Operator<'a> for FnReturnDefault {
    type Mutation = FnReturnDefaultMutation;

    fn try_apply(&self, mcx: &MutCtxt) -> Mutations<Self::Mutation> {
        let MutCtxt { tcx, def_site: def, item_hir: f_hir, location, .. } = *mcx;

        let MutLoc::Fn(f) = location else { return Mutations::none(); };

        // Returning ahead of the body is how the body is skipped: a substitution replaces an
        // expression or a statement, never a whole block.
        let Some(body) = &f.fn_data.body else { return Mutations::none(); };
        let Some(first_valid_stmt) = body.stmts.iter().find(|stmt| stmt.id != ast::DUMMY_NODE_ID) else { return Mutations::none(); };

        // Instantiated identically, so a generic function's return type stays the parameter it was
        // written as and `impls_trait` answers against that parameter's own bounds.
        let def_id = f_hir.owner_id.def_id;
        let ret_ty = tcx.fn_sig(def_id).skip_binder().output().skip_binder();

        let mut mutations = SmallVec::new();
        for replacement in replacements(tcx, def_id, def, ret_ty, 0) {
            let ret = ast::mk::stmt_expr(ast::mk::expr(def, ast::ExprKind::Ret(Some(replacement.expr))));
            mutations.push((FnReturnDefaultMutation { value: replacement.value }, smallvec![
                SubstDef::new(
                    SubstLoc::InsertBefore(first_valid_stmt.id, first_valid_stmt.span),
                    Subst::AstStmt(ret),
                ),
            ]));
        }

        Mutations::new(mutations)
    }
}
