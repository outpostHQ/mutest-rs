use mutest_emit::{Mutation, Operator};
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

    fn span_label(&self) -> String {
        self.display_name()
    }
}

/// Replace a match arm's guard with a fixed value, so the arm always or never applies.
pub struct MatchGuardValue;

impl<'a> Operator<'a> for MatchGuardValue {
    type Mutation = MatchGuardValueMutation;

    fn try_apply(&self, mcx: &MutCtxt) -> Mutations<Self::Mutation> {
        let MutCtxt { def_site: def, location, .. } = *mcx;

        let MutLoc::FnBodyExpr(expr, _) = location else { return Mutations::none(); };
        let ast::ExprKind::Match(scrutinee, arms, match_kind) = &expr.kind else { return Mutations::none(); };

        let mut mutations = SmallVec::new();
        for (i, arm) in arms.iter().enumerate() {
            if arm.guard.is_none() { continue; }

            // A guard never contributes to exhaustiveness, so a fixed value always type-checks.
            for value in [true, false] {
                let mut mutated_arms = arms.clone();
                let Some(guard) = &mut mutated_arms[i].guard else { continue };
                guard.cond = *ast::mk::expr_bool(def, value);

                let mutated = ast::mk::expr(expr.span, ast::ExprKind::Match(scrutinee.clone(), mutated_arms, *match_kind));
                mutations.push((Self::Mutation { value }, smallvec![
                    SubstDef::new(SubstLoc::Replace(expr.id, expr.span), Subst::AstExpr(*mutated)),
                ]));
            }
        }

        Mutations::new(mutations)
    }
}
