use mutest_emit::Operator;
use mutest_emit::analysis::hir;
use mutest_emit::analysis::res;
use mutest_emit::analysis::ty::{self, Ty};
use mutest_emit::codegen::ast;
use mutest_emit::codegen::mutation::{MutCtxt, MutLoc, Mutations, Subst, SubstDef, SubstLoc};
use mutest_emit::codegen::symbols::sym;
use rustc_data_structures::smallvec::smallvec;
use rustc_middle::ty::TyCtxt;

use crate::BinOpSwapMutation;

#[derive(Clone, Copy, Debug)]
pub enum OpKind {
    Standalone(ast::BinOpKind),
    Assign(ast::AssignOpKind),
}

impl OpKind {
    pub fn desc(&self) -> &str {
        match self {
            Self::Standalone(_) => "operator",
            Self::Assign(_) => "assignment operator",
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Standalone(op_kind) => op_kind.as_str(),
            Self::Assign(op_kind) => op_kind.as_str(),
        }
    }
}

fn impls_matching_op<'tcx>(tcx: TyCtxt<'tcx>, caller_def_id: hir::LocalDefId, lhs_ty: Ty<'tcx>, rhs_ty: Ty<'tcx>, expr_ty: Ty<'tcx>, op_trait: hir::DefId, op_kind: OpKind) -> bool {
    if !ty::impls_trait(tcx, caller_def_id, lhs_ty, op_trait, vec![rhs_ty.into()]) { return false; }

    match op_kind {
        OpKind::Standalone(_) => {
            ty::impl_assoc_ty(tcx, caller_def_id, lhs_ty, op_trait, vec![rhs_ty.into()], sym::Output)
                .map(|ty| ty == expr_ty).unwrap_or(false)
        }
        OpKind::Assign(_) => true,
    }
}

macro define_op_swap_operator(
    $(#[$meta:meta])*
    $vis:vis $operator:ident as $op_name_ident:ident = $op_name:literal $([$bin_op_group:expr])? {
        $(
            $bin_op_from:pat $(if impl $bin_op_to_trait:ident)? => $bin_op_to:expr
            $(, $assign_op_from:pat $(if impl $bin_assign_op_to_trait:ident)? => $assign_op_to:expr)?
        );+ $(;)?
    }
) {
    $vis const $op_name_ident: &str = $op_name;

    $(#[$meta])*
    $vis struct $operator;

    impl<'a> Operator<'a> for $operator {
        type Mutation = BinOpSwapMutation;

        fn try_apply(&self, mcx: &MutCtxt) -> Mutations<Self::Mutation> {
            let MutCtxt { opts: _, tcx, crate_res: _, def_res: _, def_site: def, item_hir: f_hir, body_res, location, value_is_borrowed: _ } = *mcx;

            let MutLoc::FnBodyExpr(expr, _f) = location else { return Mutations::none(); };

            let op_kind = match &expr.kind {
                ast::ExprKind::Binary(bin_op, _, _) => OpKind::Standalone(bin_op.node),
                ast::ExprKind::AssignOp(assign_op, _, _) => OpKind::Assign(assign_op.node),
                _ => { return Mutations::none(); }
            };

            let Some(body_hir) = f_hir.body else { return Mutations::none(); };
            let typeck = tcx.typeck_body(body_hir.id());

            let Some(expr_hir) = body_res.hir_expr(expr) else { unreachable!() };
            let expr_ty = typeck.expr_ty(expr_hir);
            let (lhs_ty, rhs_ty) = match expr_hir.kind {
                | hir::ExprKind::Binary(_, lhs, rhs)
                | hir::ExprKind::AssignOp(_, lhs, rhs) => {
                    (typeck.expr_ty(lhs), typeck.expr_ty(rhs))
                }
                _ => unreachable!(),
            };

            let caller_def_id = f_hir.owner_id.def_id;
            #[allow(unused_variables, reason = "only the optional guard arms call this closure, and an expansion may have none")]
            let expr_impls_matching_op = |op_trait| impls_matching_op(tcx, caller_def_id, lhs_ty, rhs_ty, expr_ty, op_trait, op_kind);

            let mapped_op_kind = match op_kind {
                $(
                    OpKind::Standalone($bin_op_from) $(if expr_impls_matching_op(res::traits::$bin_op_to_trait(tcx)))? => OpKind::Standalone($bin_op_to),
                    $(OpKind::Assign($assign_op_from) $(if expr_impls_matching_op(res::traits::$bin_assign_op_to_trait(tcx)))? => OpKind::Assign($assign_op_to),)?
                )+
                _ => { return Mutations::none(); }
            };

            let mapped_bin_expr = match (&expr.kind, mapped_op_kind) {
                (ast::ExprKind::Binary(_, lhs, rhs), OpKind::Standalone(mapped_bin_op)) => ast::mk::expr_binary(def, mapped_bin_op, lhs.clone(), rhs.clone()),
                (ast::ExprKind::AssignOp(_, lhs, rhs), OpKind::Assign(mapped_assign_op)) => ast::mk::expr_assign_op(def, mapped_assign_op, lhs.clone(), rhs.clone()),
                _ => unreachable!(),
            };

            let mutation = BinOpSwapMutation {
                op_name: $op_name,
                display_name: format!(concat!("swap ", $($bin_op_group, " ",)? "{op_kind} `{original_bin_op}` for `{replacement_bin_op}`"),
                    op_kind = op_kind.desc(), original_bin_op = op_kind.as_str(), replacement_bin_op = mapped_op_kind.as_str()),
                span_label: format!(concat!("swap ", $($bin_op_group, " ",)? "{op_kind} for `{replacement_bin_op}`"),
                    op_kind = op_kind.desc(), replacement_bin_op = mapped_op_kind.as_str()),
            };

            Mutations::new_one(mutation, smallvec![
                SubstDef::new(
                    SubstLoc::Replace(expr.id, expr.span),
                    Subst::AstExpr(*mapped_bin_expr),
                ),
            ])
        }
    }
}

define_op_swap_operator! {
    /// Swap addition for subtraction and vice versa.
    pub OpAddSubSwap as MATH_OP_ADD_SUB_SWAP = "math_op_add_sub_swap" {
        ast::BinOpKind::Add if impl Sub => ast::BinOpKind::Sub,
        ast::AssignOpKind::AddAssign if impl SubAssign => ast::AssignOpKind::SubAssign;
        ast::BinOpKind::Sub if impl Add => ast::BinOpKind::Add,
        ast::AssignOpKind::SubAssign if impl AddAssign => ast::AssignOpKind::AddAssign;
    }
}

define_op_swap_operator! {
    /// Swap addition for multiplication and vice versa.
    pub OpAddMulSwap as MATH_OP_ADD_MUL_SWAP = "math_op_add_mul_swap" {
        ast::BinOpKind::Add if impl Mul => ast::BinOpKind::Mul,
        ast::AssignOpKind::AddAssign if impl MulAssign => ast::AssignOpKind::MulAssign;
        ast::BinOpKind::Mul if impl Add => ast::BinOpKind::Add,
        ast::AssignOpKind::MulAssign if impl AddAssign => ast::AssignOpKind::AddAssign;
    }
}

define_op_swap_operator! {
    /// Swap multiplication for division and vice versa.
    pub OpMulDivSwap as MATH_OP_MUL_DIV_SWAP = "math_op_mul_div_swap" {
        ast::BinOpKind::Mul if impl Div => ast::BinOpKind::Div,
        ast::AssignOpKind::MulAssign if impl DivAssign => ast::AssignOpKind::DivAssign;
        ast::BinOpKind::Div if impl Mul => ast::BinOpKind::Mul,
        ast::AssignOpKind::DivAssign if impl MulAssign => ast::AssignOpKind::MulAssign;
    }
}

define_op_swap_operator! {
    /// Swap division for modulus and vice versa.
    pub OpDivRemSwap as MATH_OP_DIV_REM_SWAP = "math_op_div_rem_swap" {
        ast::BinOpKind::Div if impl Rem => ast::BinOpKind::Rem,
        ast::AssignOpKind::DivAssign if impl RemAssign => ast::AssignOpKind::RemAssign;
        ast::BinOpKind::Rem if impl Div => ast::BinOpKind::Div,
        ast::AssignOpKind::RemAssign if impl DivAssign => ast::AssignOpKind::DivAssign;
    }
}

define_op_swap_operator! {
    /// Swap bitwise OR for bitwise XOR and vice versa.
    pub BitOpOrXorSwap as BIT_OP_OR_XOR_SWAP = "bit_op_or_xor_swap" ["bitwise"] {
        ast::BinOpKind::BitOr if impl BitXor => ast::BinOpKind::BitXor,
        ast::AssignOpKind::BitOrAssign if impl BitXorAssign => ast::AssignOpKind::BitXorAssign;
        ast::BinOpKind::BitXor if impl BitOr => ast::BinOpKind::BitOr,
        ast::AssignOpKind::BitXorAssign if impl BitOrAssign => ast::AssignOpKind::BitOrAssign;
    }
}

define_op_swap_operator! {
    /// Swap bitwise OR for bitwise AND and vice versa.
    pub BitOpOrAndSwap as BIT_OP_OR_AND_SWAP = "bit_op_or_and_swap" ["bitwise"] {
        ast::BinOpKind::BitOr if impl BitAnd => ast::BinOpKind::BitAnd,
        ast::AssignOpKind::BitOrAssign if impl BitAndAssign => ast::AssignOpKind::BitAndAssign;
        ast::BinOpKind::BitAnd if impl BitOr => ast::BinOpKind::BitOr,
        ast::AssignOpKind::BitAndAssign if impl BitOrAssign => ast::AssignOpKind::BitOrAssign;
    }
}

define_op_swap_operator! {
    /// Swap bitwise XOR for bitwise AND and vice versa.
    pub BitOpXorAndSwap as BIT_OP_XOR_AND_SWAP = "bit_op_xor_and_swap" ["bitwise"] {
        ast::BinOpKind::BitXor if impl BitAnd => ast::BinOpKind::BitAnd,
        ast::AssignOpKind::BitXorAssign if impl BitAndAssign => ast::AssignOpKind::BitAndAssign;
        ast::BinOpKind::BitAnd if impl BitXor => ast::BinOpKind::BitXor,
        ast::AssignOpKind::BitAndAssign if impl BitXorAssign => ast::AssignOpKind::BitXorAssign;
    }
}

define_op_swap_operator! {
    /// Swap the direction of bitwise shift operators.
    pub BitOpShiftDirSwap as BIT_OP_SHIFT_DIR_SWAP = "bit_op_shift_dir_swap" ["bitwise"] {
        ast::BinOpKind::Shl if impl Shr => ast::BinOpKind::Shr,
        ast::AssignOpKind::ShlAssign if impl ShrAssign => ast::AssignOpKind::ShrAssign;
        ast::BinOpKind::Shr if impl Shl => ast::BinOpKind::Shl,
        ast::AssignOpKind::ShrAssign if impl ShlAssign => ast::AssignOpKind::ShlAssign;
    }
}

define_op_swap_operator! {
    /// Swap logical && for logical || and vice versa.
    pub LogicalOpAndOrSwap as LOGICAL_OP_AND_OR_SWAP = "logical_op_and_or_swap" ["logical"] {
        ast::BinOpKind::And => ast::BinOpKind::Or;
        ast::BinOpKind::Or => ast::BinOpKind::And;
    }
}
