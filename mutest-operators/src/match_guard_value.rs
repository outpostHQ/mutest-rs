use mutest_emit::{Mutation, Operator};
use mutest_emit::analysis::ast_lowering;
use mutest_emit::analysis::hir;
use mutest_emit::codegen::ast;
use mutest_emit::codegen::mutation::{MutCtxt, MutLoc, Mutations, Subst, SubstDef, SubstLoc};
use rustc_data_structures::smallvec::{SmallVec, smallvec};

pub const MATCH_GUARD_VALUE: &str = "match_guard_value";

pub struct MatchGuardValueMutation {
    pub value: bool,
}

impl Mutation for MatchGuardValueMutation {
    fn op_name(&self) -> &str { MATCH_GUARD_VALUE }

    fn display_name(&self) -> String {
        format!("replace match arm guard with `{value}`", value = self.value)
    }
}

/// Whether the guard binds names with `let` patterns, including in let chains.
fn has_let_binding(body_res: &ast_lowering::BodyResolutions<'_>, expr: &ast::Expr) -> bool {
    struct BindingFinder<'a, 'tcx> {
        body_res: &'a ast_lowering::BodyResolutions<'tcx>,
        found: bool,
    }

    impl<'ast> ast::visit::Visitor<'ast> for BindingFinder<'_, '_> {
        fn visit_pat(&mut self, pat: &'ast ast::Pat) {
            // NOTE: A binding and a unit variant (e.g. `None`) are the same AST pattern, so we check the HIR pattern.
            if let ast::PatKind::Ident(..) = pat.kind
                && self.body_res.hir_pat(pat).is_none_or(|pat_hir| matches!(pat_hir.kind, hir::PatKind::Binding(..)))
            {
                self.found = true;
            }
            ast::visit::walk_pat(self, pat);
        }
    }

    match &expr.kind {
        ast::ExprKind::Let(pat, ..) => {
            let mut finder = BindingFinder { body_res, found: false };
            ast::visit::Visitor::visit_pat(&mut finder, pat);
            finder.found
        }
        ast::ExprKind::Binary(_, lhs, rhs) => has_let_binding(body_res, lhs) || has_let_binding(body_res, rhs),
        ast::ExprKind::Paren(inner) => has_let_binding(body_res, inner),
        _ => false,
    }
}

/// Replace boolean match arm guards with fixed values.
pub struct MatchGuardValue;

impl<'a> Operator<'a> for MatchGuardValue {
    type Mutation = MatchGuardValueMutation;

    fn try_apply(&self, mcx: &MutCtxt) -> Mutations<Self::Mutation> {
        let MutCtxt { opts: _, tcx: _, crate_res: _, def_res: _, def_site: def, item_hir: _, body_res, location, value_is_borrowed: _ } = *mcx;

        let MutLoc::FnBodyExpr(expr, _f) = location else { return Mutations::none(); };
        let ast::ExprKind::Match(scrutinee, arms, match_kind) = &expr.kind else { return Mutations::none(); };

        let mut mutations = SmallVec::new();
        for (i, arm) in arms.iter().enumerate() {
            let Some(guard) = &arm.guard else { continue; };
            // NOTE: The arm uses the names that `let` guards bind, so those guards must stay.
            if has_let_binding(body_res, &guard.cond) { continue; }

            // NOTE: Guards do not count towards exhaustiveness, so either value keeps the match valid.
            for value in [true, false] {
                let mut mutated_arms = arms.clone();
                let Some(mutated_guard) = &mut mutated_arms[i].guard else { unreachable!() };
                mutated_guard.cond = *ast::mk::expr_bool(def, value);
                let mutated_expr = ast::mk::expr(expr.span, ast::ExprKind::Match(scrutinee.clone(), mutated_arms, *match_kind));

                mutations.push((Self::Mutation { value }, smallvec![
                    SubstDef::new(
                        SubstLoc::Replace(expr.id, expr.span),
                        Subst::AstExpr(*mutated_expr),
                    ),
                ]));
            }
        }

        Mutations::new(mutations)
    }
}
