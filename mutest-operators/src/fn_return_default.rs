use mutest_emit::{Mutation, Operator};
use mutest_emit::analysis::hir;
use mutest_emit::analysis::res;
use mutest_emit::analysis::ty::{self, Ty, TyCtxt, TyKind};
use mutest_emit::codegen::ast;
use mutest_emit::codegen::mutation::{MutCtxt, MutLoc, Mutations, Subst, SubstDef, SubstLoc};
use mutest_emit::codegen::symbols::{Span, Symbol, path, sym};
use rustc_data_structures::smallvec::{SmallVec, smallvec};
use rustc_data_structures::thin_vec::thin_vec;

pub const FN_RETURN_DEFAULT: &str = "fn_return_default";

pub struct FnReturnDefaultMutation {
    pub value: String,
}

impl Mutation for FnReturnDefaultMutation {
    fn op_name(&self) -> &str { FN_RETURN_DEFAULT }

    fn display_name(&self) -> String {
        format!("return `{value}` without evaluating the function body", value = self.value)
    }
}

/// A value to return, with the source text it is displayed as.
struct Replacement {
    value: String,
    expr: Box<ast::Expr>,
}

impl Replacement {
    fn new(value: impl Into<String>, expr: Box<ast::Expr>) -> Self {
        Self { value: value.into(), expr }
    }
}

/// How many levels of `Option`, `Result` and `Vec` return types are unwrapped.
const MAX_TY_DEPTH: u32 = 3;

struct Replacements<'tcx> {
    tcx: TyCtxt<'tcx>,
    body_def_id: hir::LocalDefId,
    sp: Span,
}

impl<'tcx> Replacements<'tcx> {
    /// Values of type `ty` for the function to return instead of evaluating its body.
    fn of(&self, ty: Ty<'tcx>, depth: u32) -> Vec<Replacement> {
        let (tcx, sp) = (self.tcx, self.sp);

        if depth > MAX_TY_DEPTH { return self.default_value(ty); }

        match ty.kind() {
            // NOTE: With only the default value, a body that always returns the other value would go undetected.
            TyKind::Bool => vec![
                Replacement::new("false", ast::mk::expr_bool(sp, false)),
                Replacement::new("true", ast::mk::expr_bool(sp, true)),
            ],
            TyKind::Int(_) => vec![
                Replacement::new("0", ast::mk::expr_int(sp, 0)),
                Replacement::new("1", ast::mk::expr_int(sp, 1)),
                Replacement::new("-1", ast::mk::expr_int(sp, -1)),
            ],
            TyKind::Uint(_) => vec![
                Replacement::new("0", ast::mk::expr_int(sp, 0)),
                Replacement::new("1", ast::mk::expr_int(sp, 1)),
            ],
            TyKind::Float(_) => vec![
                Replacement::new("0.0", expr_float(sp, "0.0")),
                Replacement::new("1.0", expr_float(sp, "1.0")),
                Replacement::new("-1.0", ast::mk::expr_unary(sp, ast::UnOp::Neg, expr_float(sp, "1.0"))),
            ],
            TyKind::Tuple(elems) if elems.is_empty() => vec![
                Replacement::new("()", ast::mk::expr_tuple(sp, thin_vec![])),
            ],
            // NOTE: A `&'static str` satisfies any shorter lifetime in the signature.
            TyKind::Ref(_, inner, ast::Mutability::Not) if inner.is_str() => vec![
                Replacement::new(r#""""#, ast::mk::expr_str(sp, "")),
                Replacement::new(r#""xyzzy""#, ast::mk::expr_str(sp, "xyzzy")),
            ],
            // NOTE: `Result` does not implement `Default`, so we return its success variant.
            TyKind::Adt(adt_def, args) if tcx.is_diagnostic_item(sym::Result, adt_def.did()) => {
                self.wrapped(args.type_at(0), depth, path::Ok(sp), "Ok")
            }
            TyKind::Adt(adt_def, args) if tcx.is_diagnostic_item(sym::Option, adt_def.did()) => {
                let mut replacements = vec![Replacement::new("None", ast::mk::expr_path(path::None(sp)))];
                replacements.extend(self.wrapped(args.type_at(0), depth, path::Some(sp), "Some"));
                replacements
            }
            TyKind::Adt(adt_def, args) if tcx.is_diagnostic_item(sym::Vec, adt_def.did()) => {
                self.vec(ty, *adt_def, args, depth)
            }
            // NOTE: `String` is a lang item rather than a diagnostic item.
            TyKind::Adt(adt_def, _) if tcx.lang_items().string() == Some(adt_def.did()) => vec![
                Replacement::new("String::new()", expr_default(sp)),
                Replacement::new(r#"String::from("xyzzy")"#, ast::mk::expr_call_path(sp, path::string_from(sp), thin_vec![ast::mk::expr_str(sp, "xyzzy")])),
            ],
            _ => self.default_value(ty),
        }
    }

    /// The values of `inner`, each wrapped in the variant `ctor`, e.g. `Ok(false)` and `Ok(true)`.
    fn wrapped(&self, inner: Ty<'tcx>, depth: u32, ctor: ast::Path, name: &str) -> Vec<Replacement> {
        self.of(inner, depth + 1).into_iter()
            .map(|r| Replacement::new(format!("{name}({})", r.value), ast::mk::expr_call_path(self.sp, ctor.clone(), thin_vec![r.expr])))
            .collect()
    }

    fn vec(&self, vec_ty: Ty<'tcx>, vec_def: ty::AdtDef<'tcx>, args: ty::GenericArgsRef<'tcx>, depth: u32) -> Vec<Replacement> {
        // NOTE: `Vec::from([..])` only builds vectors with the default allocator.
        if !self.has_default_allocator(vec_def, args) { return self.default_value(vec_ty); }

        let mut replacements = match self.has_default(vec_ty) {
            true => vec![Replacement::new("Vec::new()", expr_default(self.sp))],
            false => vec![],
        };
        replacements.extend(self.of(args.type_at(0), depth + 1).into_iter().map(|r| {
            Replacement::new(format!("Vec::from([{}])", r.value), ast::mk::expr_call_path(self.sp, path::vec_from(self.sp), thin_vec![ast::mk::expr_array(self.sp, thin_vec![r.expr])]))
        }));
        replacements
    }

    fn has_default_allocator(&self, vec_def: ty::AdtDef<'tcx>, args: ty::GenericArgsRef<'tcx>) -> bool {
        let Some(allocator_param) = self.tcx.generics_of(vec_def.did()).own_params.get(1) else { return false; };
        let ty::GenericParamDefKind::Type { has_default: true, .. } = allocator_param.kind else { return false; };
        let Some(allocator) = args.get(1).and_then(|arg| arg.as_type()) else { return false; };
        allocator == self.tcx.type_of(allocator_param.def_id).instantiate(self.tcx, args).skip_normalization()
    }

    fn default_value(&self, ty: Ty<'tcx>) -> Vec<Replacement> {
        match self.has_default(ty) {
            true => vec![Replacement::new("Default::default()", expr_default(self.sp))],
            false => vec![],
        }
    }

    fn has_default(&self, ty: Ty<'tcx>) -> bool {
        ty::impls_trait(self.tcx, self.body_def_id, ty, res::traits::Default(self.tcx), vec![])
    }
}

fn expr_default(sp: Span) -> Box<ast::Expr> {
    ast::mk::expr_call_path(sp, path::default(sp), thin_vec![])
}

fn expr_float(sp: Span, value: &str) -> Box<ast::Expr> {
    ast::mk::expr_lit(sp, ast::token::LitKind::Float, Symbol::intern(value), None)
}

/// Replace function bodies with a fixed return value.
pub struct FnReturnDefault;

impl<'a> Operator<'a> for FnReturnDefault {
    type Mutation = FnReturnDefaultMutation;

    fn try_apply(&self, mcx: &MutCtxt) -> Mutations<Self::Mutation> {
        let MutCtxt { opts: _, tcx, crate_res: _, def_res: _, def_site: def, item_hir: f_hir, body_res: _, location, value_is_borrowed: _ } = *mcx;

        let MutLoc::Fn(f) = location else { return Mutations::none(); };

        // NOTE: Substitutions target statements, so we return before the first statement of the body.
        let Some(body) = &f.fn_data.body else { return Mutations::none(); };
        let Some(first_stmt) = body.stmts.iter().find(|stmt| stmt.id != ast::DUMMY_NODE_ID) else { return Mutations::none(); };

        // NOTE: The signature is left uninstantiated, so trait checks use the function's own bounds.
        let def_id = f_hir.owner_id.def_id;
        let ret_ty = tcx.fn_sig(def_id).skip_binder().output().skip_binder();

        let replacements = Replacements { tcx, body_def_id: def_id, sp: def };
        let mutations = replacements.of(ret_ty, 0).into_iter()
            .map(|replacement| {
                let ret_stmt = ast::mk::stmt_expr(ast::mk::expr(def, ast::ExprKind::Ret(Some(replacement.expr))));
                (FnReturnDefaultMutation { value: replacement.value }, smallvec![
                    SubstDef::new(SubstLoc::InsertBefore(first_stmt.id, first_stmt.span), Subst::AstStmt(ret_stmt)),
                ])
            })
            .collect::<SmallVec<_>>();

        Mutations::new(mutations)
    }
}
