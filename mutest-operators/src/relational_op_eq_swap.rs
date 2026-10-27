use mutest_emit::Operator;
use mutest_emit::codegen::ast;
use mutest_emit::codegen::mutation::{MutCtxt, Mutations};

use crate::{BinOpSwapMutation, swap_bin_op};

pub const RELATIONAL_OP_EQ_SWAP: &str = "relational_op_eq_swap";

/// The mutation that adds the equality to the relational operator, or removes it.
fn eq_swap(original_bin_op: ast::BinOpKind, replacement_bin_op: ast::BinOpKind) -> BinOpSwapMutation {
    let operation = match replacement_bin_op {
        ast::BinOpKind::Le | ast::BinOpKind::Ge => "add equality to",
        ast::BinOpKind::Lt | ast::BinOpKind::Gt => "remove equality from",
        _ => unreachable!(),
    };
    let display_name = format!("{operation} relational operator `{original_bin_op}`", original_bin_op = original_bin_op.as_str());
    BinOpSwapMutation { op_name: RELATIONAL_OP_EQ_SWAP, display_name, span_label: format!("{operation} relational operator") }
}

/// Include or remove the boundary (equality) of relational operators.
pub struct RelationalOpEqSwap;

impl<'a> Operator<'a> for RelationalOpEqSwap {
    type Mutation = BinOpSwapMutation;

    fn try_apply(&self, mcx: &MutCtxt) -> Mutations<Self::Mutation> {
        swap_bin_op(mcx, |bin_op| match bin_op {
            ast::BinOpKind::Lt => Some(ast::BinOpKind::Le),
            ast::BinOpKind::Le => Some(ast::BinOpKind::Lt),
            ast::BinOpKind::Gt => Some(ast::BinOpKind::Ge),
            ast::BinOpKind::Ge => Some(ast::BinOpKind::Gt),
            _ => None,
        }, eq_swap)
    }
}
