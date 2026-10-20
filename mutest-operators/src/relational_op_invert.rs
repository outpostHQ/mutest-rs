use mutest_emit::Operator;
use mutest_emit::codegen::ast;
use mutest_emit::codegen::mutation::{MutCtxt, MutLoc, Mutations};

use crate::{BinOpSwapMutation, invert_bin_op, swap_bin_op};

pub const RELATIONAL_OP_INVERT: &str = "relational_op_invert";

/// Completely invert relational operators.
pub struct RelationalOpInvert;

impl<'a> Operator<'a> for RelationalOpInvert {
    type Mutation = BinOpSwapMutation;

    fn try_apply(&self, mcx: &MutCtxt) -> Mutations<Self::Mutation> {
        let MutCtxt { opts: _, tcx: _, crate_res: _, def_res: _, def_site: def, item_hir: _, body_res: _, location, value_is_borrowed: _ } = *mcx;

        let MutLoc::FnBodyExpr(expr, _) = location else { return Mutations::none(); };

        swap_bin_op(def, expr, |bin_op| match bin_op {
            ast::BinOpKind::Lt => Some(ast::BinOpKind::Ge),
            ast::BinOpKind::Le => Some(ast::BinOpKind::Gt),
            ast::BinOpKind::Gt => Some(ast::BinOpKind::Le),
            ast::BinOpKind::Ge => Some(ast::BinOpKind::Lt),
            _ => None,
        }, |original_bin_op, replacement_bin_op| invert_bin_op(RELATIONAL_OP_INVERT, "relational", original_bin_op, replacement_bin_op))
    }
}
