use mutest_emit::{Mutation, Operator};
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

    fn span_label(&self) -> String {
        self.display_name()
    }
}

/// Delete a `!` or a `-`, so the operand is used unchanged.
pub struct UnaryOpDelete;

impl<'a> Operator<'a> for UnaryOpDelete {
    type Mutation = UnaryOpDeleteMutation;

    fn try_apply(&self, mcx: &MutCtxt) -> Mutations<Self::Mutation> {
        let MutCtxt { location, .. } = *mcx;

        let MutLoc::FnBodyExpr(expr, _) = location else { return Mutations::none(); };
        let ast::ExprKind::Unary(un_op, operand) = &expr.kind else { return Mutations::none(); };

        // `*` changes the type, so dropping it does not compile — and every mutant shares one binary.
        let op = match un_op {
            ast::UnOp::Not => "!",
            ast::UnOp::Neg => "-",
            ast::UnOp::Deref => return Mutations::none(),
        };

        Mutations::new_one(UnaryOpDeleteMutation { op }, smallvec![
            SubstDef::new(SubstLoc::Replace(expr.id, expr.span), Subst::AstExpr((**operand).clone())),
        ])
    }
}
