use mutest_emit::{Mutation, Operator};
use mutest_emit::analysis::res;
use mutest_emit::analysis::ty;
use mutest_emit::codegen::ast;
use mutest_emit::codegen::mutation::{MutCtxt, MutLoc, Mutations, Subst, SubstDef, SubstLoc};
use mutest_emit::codegen::symbols::path;
use rustc_data_structures::smallvec::smallvec;
use rustc_data_structures::thin_vec::thin_vec;

pub const FN_RETURN_DEFAULT: &str = "fn_return_default";

pub struct FnReturnDefaultMutation;

impl Mutation for FnReturnDefaultMutation {
    fn op_name(&self) -> &str { FN_RETURN_DEFAULT }

    fn display_name(&self) -> String {
        "return `Default::default()` without evaluating the function body".to_owned()
    }

    fn span_label(&self) -> String {
        self.display_name()
    }
}

/// Return `Default::default()` from a function without running its body, to check that some test
/// depends on what the function computes rather than on it merely being called.
pub struct FnReturnDefault;

impl<'a> Operator<'a> for FnReturnDefault {
    type Mutation = FnReturnDefaultMutation;

    fn try_apply(&self, mcx: &MutCtxt) -> Mutations<Self::Mutation> {
        let MutCtxt { tcx, def_site: def, item_hir: f_hir, location, .. } = *mcx;

        let MutLoc::Fn(f) = location else { return Mutations::none(); };

        // Returning ahead of the body is how the body is skipped: a substitution replaces an
        // expression or a statement, never a whole block.
        let Some(body) = &f.fn_data.body else { return Mutations::none(); };
        let Some(first_valid_stmt) = body.stmts.iter().find(|stmt| stmt.id != ast::DUMMY_NODE_ID) else { return Mutations::none(); };

        // Instantiated identically, so a generic function's return type stays the parameter it was
        // written as and `impls_trait` answers against that parameter's own bounds.
        let ret_ty = tcx.fn_sig(f_hir.owner_id.def_id).skip_binder().output().skip_binder();
        if !ty::impls_trait(tcx, f_hir.owner_id.def_id, ret_ty, res::traits::Default(tcx), vec![]) { return Mutations::none(); }

        let default = ast::mk::expr_call_path(def, path::default(def), thin_vec![]);
        let ret = ast::mk::stmt_expr(ast::mk::expr(def, ast::ExprKind::Ret(Some(default))));

        Mutations::new_one(FnReturnDefaultMutation, smallvec![
            SubstDef::new(
                SubstLoc::InsertBefore(first_valid_stmt.id, first_valid_stmt.span),
                Subst::AstStmt(ret),
            ),
        ])
    }
}
