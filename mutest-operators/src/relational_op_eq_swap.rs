use mutest_emit::{Mutation, Operator};
use mutest_emit::codegen::ast;
use mutest_emit::codegen::mutation::{MutCtxt, MutLoc, Mutations};

use crate::swap_bin_op;

pub const RELATIONAL_OP_EQ_SWAP: &str = "relational_op_eq_swap";

pub struct RelationalOpEqSwapMutation {
    pub original_bin_op: ast::BinOpKind,
    pub replacement_bin_op: ast::BinOpKind,
}

impl Mutation for RelationalOpEqSwapMutation {
    fn op_name(&self) -> &str { RELATIONAL_OP_EQ_SWAP }

    fn display_name(&self) -> String {
        format!("{operation} relational operator `{original_bin_op}`",
            operation = match self.replacement_bin_op {
                ast::BinOpKind::Le | ast::BinOpKind::Ge => "add equality to",
                ast::BinOpKind::Lt | ast::BinOpKind::Gt => "remove equality from",
                _ => unreachable!(),
            },
            original_bin_op = self.original_bin_op.as_str(),
        )
    }

    fn span_label(&self) -> String {
        format!("{operation} relational operator",
            operation = match self.replacement_bin_op {
                ast::BinOpKind::Le | ast::BinOpKind::Ge => "add equality to",
                ast::BinOpKind::Lt | ast::BinOpKind::Gt => "remove equality from",
                _ => unreachable!(),
            },
        )
    }
}

/// Include or remove the boundary (equality) of relational operators.
pub struct RelationalOpEqSwap;

impl<'a> Operator<'a> for RelationalOpEqSwap {
    type Mutation = RelationalOpEqSwapMutation;

    fn try_apply(&self, mcx: &MutCtxt) -> Mutations<Self::Mutation> {
        let MutCtxt { opts: _, tcx: _, crate_res: _, def_res: _, def_site: def, item_hir: _, body_res: _, location, value_is_borrowed: _ } = *mcx;

        let MutLoc::FnBodyExpr(expr, _) = location else { return Mutations::none(); };

        swap_bin_op(def, expr, |bin_op| match bin_op {
            ast::BinOpKind::Lt => Some(ast::BinOpKind::Le),
            ast::BinOpKind::Le => Some(ast::BinOpKind::Lt),
            ast::BinOpKind::Gt => Some(ast::BinOpKind::Ge),
            ast::BinOpKind::Ge => Some(ast::BinOpKind::Gt),
            _ => None,
        }, |original_bin_op, replacement_bin_op| Self::Mutation { original_bin_op, replacement_bin_op })
    }
}
