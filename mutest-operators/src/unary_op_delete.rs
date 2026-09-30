use mutest_emit::{Mutation, Operator};
use mutest_emit::analysis::hir;
use mutest_emit::codegen::ast;
use mutest_emit::codegen::mutation::{MutCtxt, MutLoc, Mutations, Subst, SubstDef, SubstLoc};
use rustc_data_structures::smallvec::smallvec;

pub const UNARY_OP_DELETE: &str = "unary_op_delete";

pub struct UnaryOpDeleteMutation {
    pub op: &'static str,
}

impl Mutation for UnaryOpDeleteMutation {
    fn op_name(&self) -> &str { UNARY_OP_DELETE }

    fn display_name(&self) -> String {
        format!("delete unary operator `{op}`", op = self.op)
    }
}

/// Delete `!` and `-` unary operators, so the operand is used unchanged.
pub struct UnaryOpDelete;

impl<'a> Operator<'a> for UnaryOpDelete {
    type Mutation = UnaryOpDeleteMutation;

    fn try_apply(&self, mcx: &MutCtxt) -> Mutations<Self::Mutation> {
        let MutCtxt { opts: _, tcx, crate_res: _, def_res: _, def_site: _, item_hir: f_hir, body_res, location, value_is_borrowed: _ } = *mcx;

        let MutLoc::FnBodyExpr(expr, _f) = location else { return Mutations::none(); };
        let ast::ExprKind::Unary(un_op, operand) = &expr.kind else { return Mutations::none(); };
        let op = match un_op {
            ast::UnOp::Not => "!",
            ast::UnOp::Neg => "-",
            ast::UnOp::Deref => return Mutations::none(),
        };

        let Some(body_hir) = f_hir.body else { return Mutations::none(); };
        let typeck = tcx.typeck_body(body_hir.id());
        let Some(expr_hir) = body_res.hir_expr(expr) else { return Mutations::none(); };
        let hir::ExprKind::Unary(_, operand_hir) = expr_hir.kind else { return Mutations::none(); };
        // NOTE: Overloaded operators may return a type other than their operand's.
        if typeck.expr_ty(operand_hir) != typeck.expr_ty(expr_hir) { return Mutations::none(); }

        Mutations::new_one(Self::Mutation { op }, smallvec![
            SubstDef::new(
                SubstLoc::Replace(expr.id, expr.span),
                Subst::AstExpr((**operand).clone()),
            ),
        ])
    }
}
