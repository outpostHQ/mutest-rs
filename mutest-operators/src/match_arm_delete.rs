use mutest_emit::{Mutation, Operator};
use mutest_emit::analysis::ast_lowering;
use mutest_emit::analysis::hir;
use mutest_emit::codegen::ast;
use mutest_emit::codegen::mutation::{MutCtxt, MutLoc, Mutations, Subst, SubstDef, SubstLoc};
use mutest_emit::codegen::symbols::Span;
use rustc_data_structures::smallvec::{SmallVec, smallvec};

pub const MATCH_ARM_DELETE: &str = "match_arm_delete";

pub struct MatchArmDeleteMutation {
    pub arm_pat: String,
}

impl Mutation for MatchArmDeleteMutation {
    fn op_name(&self) -> &str { MATCH_ARM_DELETE }

    fn display_name(&self) -> String {
        format!("delete match arm `{arm_pat}`", arm_pat = self.arm_pat)
    }

    fn span_label(&self) -> String {
        "delete match arm".to_owned()
    }
}

/// Whether the pattern matches every value.
// NOTE: A binding and a unit variant are the same AST pattern, so we check the HIR pattern.
fn is_irrefutable(body_res: &ast_lowering::BodyResolutions, pat: &ast::Pat) -> bool {
    if let ast::PatKind::Paren(inner) = &pat.kind { return is_irrefutable(body_res, inner); }
    let Some(pat_hir) = body_res.hir_pat(pat) else { return false; };
    matches!(pat_hir.kind, hir::PatKind::Wild | hir::PatKind::Binding(_, _, _, None))
}

/// Puts `false &&` at the start of the guard's `&&` chain, so that a let chain stays one chain and keeps its bindings.
fn falsify_guard(def: Span, cond: &mut ast::Expr) {
    if let ast::ExprKind::Binary(op, lhs, _) = &mut cond.kind && op.node == ast::BinOpKind::And { return falsify_guard(def, lhs); }
    *cond = *ast::mk::expr_binary(def, ast::BinOpKind::And, ast::mk::expr_bool(def, false), Box::new(cond.clone()));
}

/// Delete match arms, so the values they matched fall through to a later arm.
pub struct MatchArmDelete;

impl<'a> Operator<'a> for MatchArmDelete {
    type Mutation = MatchArmDeleteMutation;

    fn try_apply(&self, mcx: &MutCtxt) -> Mutations<Self::Mutation> {
        let MutCtxt { opts: _, tcx: _, crate_res: _, def_res: _, def_site: def, item_hir: _, body_res, location, value_is_borrowed: _ } = *mcx;

        let MutLoc::FnBodyExpr(expr, _f) = location else { return Mutations::none(); };
        let ast::ExprKind::Match(scrutinee, arms, match_kind) = &expr.kind else { return Mutations::none(); };

        // NOTE: Only arms before an unguarded catch-all arm can be deleted without breaking exhaustiveness.
        let Some(last_catch_all) = arms.iter().rposition(|arm| arm.guard.is_none() && is_irrefutable(body_res, &arm.pat)) else { return Mutations::none(); };

        let mutations = arms[..last_catch_all].iter().enumerate()
            .filter(|(_, arm)| arm.body.is_some())
            .map(|(i, arm)| {
                // NOTE: A guard that never holds deletes the arm, but keeps its body, whose type the other arms may coerce to.
                let mut mutated_arms = arms.clone();
                match &mut mutated_arms[i].guard {
                    Some(guard) => falsify_guard(def, &mut guard.cond),
                    guard @ None => *guard = Some(Box::new(ast::Guard { cond: *ast::mk::expr_bool(def, false), span_with_leading_if: def })),
                }
                let mutated_expr = ast::mk::expr(expr.span, ast::ExprKind::Match(scrutinee.clone(), mutated_arms, *match_kind));

                let mutation = Self::Mutation { arm_pat: ast::print::pat_to_string(&arm.pat) };
                (mutation, smallvec![
                    SubstDef::new(
                        SubstLoc::Replace(expr.id, expr.span),
                        Subst::AstExpr(*mutated_expr),
                    ),
                ])
            })
            .collect::<SmallVec<_>>();

        Mutations::new(mutations)
    }
}
