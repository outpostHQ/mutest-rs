use mutest_emit::{Mutation, Operator};
use mutest_emit::analysis::ast_lowering;
use mutest_emit::analysis::hir;
use mutest_emit::codegen::ast;
use mutest_emit::codegen::mutation::{MutCtxt, MutLoc, Mutations, Subst, SubstDef, SubstLoc};
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

/// A bare identifier pattern is a binding or a unit variant path, and only resolution knows which,
/// so the HIR is the authority: `None` parses exactly like `x` does.
fn is_irrefutable(body_res: &ast_lowering::BodyResolutions, pat: &ast::Pat) -> bool {
    if let ast::PatKind::Paren(inner) = &pat.kind { return is_irrefutable(body_res, inner); }
    let Some(pat_hir) = body_res.hir_pat(pat) else { return false };
    matches!(pat_hir.kind, hir::PatKind::Wild | hir::PatKind::Binding(_, _, _, None))
}

/// Delete a match arm, so the value it matched falls through to a later arm.
pub struct MatchArmDelete;

impl<'a> Operator<'a> for MatchArmDelete {
    type Mutation = MatchArmDeleteMutation;

    fn try_apply(&self, mcx: &MutCtxt) -> Mutations<Self::Mutation> {
        let MutCtxt { body_res, location, .. } = *mcx;

        let MutLoc::FnBodyExpr(expr, _) = location else { return Mutations::none(); };
        let ast::ExprKind::Match(scrutinee, arms, match_kind) = &expr.kind else { return Mutations::none(); };

        // Every mutant compiles into one binary, so a mutation that does not compile breaks the
        // whole run — there is no unviable-mutant outcome here as there is in cargo-mutants.
        let Some(last_catch_all) = arms.iter().rposition(|arm| arm.guard.is_none() && is_irrefutable(body_res, &arm.pat)) else { return Mutations::none(); };

        let mut mutations = SmallVec::new();
        for (i, arm) in arms.iter().enumerate() {
            if i >= last_catch_all { continue; }
            if arm.body.is_none() { continue; }

            let kept = arms.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, a)| a.clone()).collect();
            let mutated = ast::mk::expr(expr.span, ast::ExprKind::Match(scrutinee.clone(), kept, *match_kind));

            let mutation = Self::Mutation { arm_pat: ast::print::pat_to_string(&arm.pat) };
            mutations.push((mutation, smallvec![
                SubstDef::new(SubstLoc::Replace(expr.id, expr.span), Subst::AstExpr(*mutated)),
            ]));
        }

        Mutations::new(mutations)
    }
}
