use mutest_emit::{Mutation, Operator};
use mutest_emit::codegen::ast;
use mutest_emit::codegen::mutation::{MutCtxt, MutLoc, Mutations, Subst, SubstDef, SubstLoc};
use rustc_data_structures::smallvec::smallvec;

pub const EQ_OP_INVERT: &str = "eq_op_invert";

/// A mutation that swaps a binary operator: the name of its operator, and the texts that describe the swap.
pub struct BinOpSwapMutation {
    pub op_name: &'static str,
    pub display_name: String,
    pub span_label: String,
}

impl Mutation for BinOpSwapMutation {
    fn op_name(&self) -> &str { self.op_name }
    fn display_name(&self) -> String { self.display_name.clone() }
    fn span_label(&self) -> String { self.span_label.clone() }
}

/// The mutation that inverts an operator of the kind, such as `equality`, to the replacement.
pub(crate) fn invert_bin_op(op_name: &'static str, kind: &str, original_bin_op: ast::BinOpKind, replacement_bin_op: ast::BinOpKind) -> BinOpSwapMutation {
    let (original_bin_op, replacement_bin_op) = (original_bin_op.as_str(), replacement_bin_op.as_str());
    let display_name = format!("invert {kind} operator `{original_bin_op}` to `{replacement_bin_op}`");
    BinOpSwapMutation { op_name, display_name, span_label: format!("invert {kind} operator to `{replacement_bin_op}`") }
}

/// Invert equality checks.
pub struct EqOpInvert;

impl<'a> Operator<'a> for EqOpInvert {
    type Mutation = BinOpSwapMutation;

    fn try_apply(&self, mcx: &MutCtxt) -> Mutations<Self::Mutation> {
        swap_bin_op(mcx, |bin_op| match bin_op {
            ast::BinOpKind::Eq => Some(ast::BinOpKind::Ne),
            ast::BinOpKind::Ne => Some(ast::BinOpKind::Eq),
            _ => None,
        }, |original_bin_op, replacement_bin_op| invert_bin_op(EQ_OP_INVERT, "equality", original_bin_op, replacement_bin_op))
    }
}

/// The mutation of a binary expression in a function body whose operator `swap` maps to another one.
pub(crate) fn swap_bin_op<M: Mutation>(mcx: &MutCtxt, swap: fn(ast::BinOpKind) -> Option<ast::BinOpKind>, mutation: fn(ast::BinOpKind, ast::BinOpKind) -> M) -> Mutations<M> {
    let MutCtxt { opts: _, tcx: _, crate_res: _, def_res: _, def_site: def, item_hir: _, body_res: _, location, value_is_borrowed: _ } = *mcx;
    let MutLoc::FnBodyExpr(expr, _) = location else { return Mutations::none(); };
    let ast::ExprKind::Binary(bin_op, lhs, rhs) = &expr.kind else { return Mutations::none(); };
    let Some(replacement_bin_op) = swap(bin_op.node) else { return Mutations::none(); };
    let replacement_bin_expr = ast::mk::expr_binary(def, replacement_bin_op, lhs.clone(), rhs.clone());

    Mutations::new_one(mutation(bin_op.node, replacement_bin_op), smallvec![
        SubstDef::new(SubstLoc::Replace(expr.id, expr.span), Subst::AstExpr(*replacement_bin_expr)),
    ])
}
