use mutest_emit::{Mutation, Operator};
use mutest_emit::analysis::hir;
use mutest_emit::analysis::ty;
use mutest_emit::codegen::ast;
use mutest_emit::codegen::mutation::{MutCtxt, MutLoc, Mutations, Subst, SubstDef, SubstLoc};
use rustc_data_structures::smallvec::smallvec;
use rustc_middle::ty::TyCtxt;

pub const CONTINUE_BREAK_SWAP: &str = "continue_break_swap";

pub struct ContinueBreakSwapMutation {
    pub original_expr: ast::ExprKind,
    pub replacement_expr: ast::ExprKind,
}

impl Mutation for ContinueBreakSwapMutation {
    fn op_name(&self) -> &str { CONTINUE_BREAK_SWAP }

    fn display_name(&self) -> String {
        let display_expr = |expr: &ast::ExprKind| match expr {
            ast::ExprKind::Break(Some(label), _) => format!("break with label `{}`", label.ident),
            ast::ExprKind::Break(None, _) => "break".to_owned(),
            ast::ExprKind::Continue(Some(label)) => format!("continue with label `{}`", label.ident),
            ast::ExprKind::Continue(None) => "continue".to_owned(),
            _ => unreachable!(),
        };

        format!("swap {original_expr} for {replacement_expr}",
            original_expr = display_expr(&self.original_expr),
            replacement_expr = display_expr(&self.replacement_expr),
        )
    }

    fn span_label(&self) -> String {
        let display_expr = |expr: &ast::ExprKind| match expr {
            ast::ExprKind::Break(Some(label), _) => format!("break with label `{}`", label.ident),
            ast::ExprKind::Break(None, _) => "break".to_owned(),
            ast::ExprKind::Continue(Some(label)) => format!("continue with label `{}`", label.ident),
            ast::ExprKind::Continue(None) => "continue".to_owned(),
            _ => unreachable!(),
        };

        format!("swap for {replacement_expr}",
            replacement_expr = display_expr(&self.replacement_expr),
        )
    }
}

/// Whether a `loop` of type `!` still type-checks as `()` that may finish: its value coerces to `()`,
/// or it is a statement that code after it does not rely on to diverge.
fn loop_may_become_unit<'tcx>(tcx: TyCtxt<'tcx>, typeck: &ty::TypeckResults<'tcx>, loop_hir: &'tcx hir::Expr<'tcx>) -> bool {
    if typeck.expr_ty_adjusted(loop_hir) == tcx.types.unit { return true; }

    let hir::Node::Stmt(stmt) = tcx.parent_hir_node(loop_hir.hir_id) else { return false; };
    let hir::Node::Block(block) = tcx.parent_hir_node(stmt.hir_id) else { return false; };
    let last_in_block = block.expr.is_none() && block.stmts.last().is_some_and(|last| last.hir_id == stmt.hir_id);
    if !last_in_block { return true; }

    // The block ends in the loop, so the block's value is the loop's divergence.
    let hir::Node::Expr(block_expr) = tcx.parent_hir_node(block.hir_id) else { return false; };
    typeck.expr_ty_adjusted(block_expr) == tcx.types.unit
}

/// Whether the loop moves a value declared outside it. A `break` after such a move leaves the loop
/// before it could run again; a `continue` in its place would take the moved value round to the next
/// iteration, which borrowck rejects (E0382) wherever that iteration uses or moves it again.
fn loop_moves_outer_value<'tcx>(tcx: TyCtxt<'tcx>, typeck: &'tcx ty::TypeckResults<'tcx>, body_owner: hir::LocalDefId, body_id: hir::BodyId, loop_hir: &'tcx hir::Expr<'tcx>) -> bool {
    use rustc_hir_typeck::expr_use_visitor::{Delegate, ExprUseVisitor, PlaceBase, PlaceWithHirId};
    use rustc_lint::LateContext;
    use rustc_middle::mir::FakeReadCause;

    struct OuterMoveFinder<'tcx> {
        tcx: TyCtxt<'tcx>,
        loop_hir_id: hir::HirId,
        found: bool,
    }

    impl<'tcx> Delegate<'tcx> for OuterMoveFinder<'tcx> {
        fn consume(&mut self, place_with_id: &PlaceWithHirId<'tcx>, _diag_expr_id: hir::HirId) {
            let declared_at = match place_with_id.place.base {
                PlaceBase::Local(hir_id) => hir_id,
                PlaceBase::Upvar(upvar_id) => upvar_id.var_path.hir_id,
                PlaceBase::Rvalue | PlaceBase::StaticItem => return,
            };
            if !self.tcx.hir_parent_id_iter(declared_at).any(|hir_id| hir_id == self.loop_hir_id) {
                self.found = true;
            }
        }

        fn use_cloned(&mut self, _place_with_id: &PlaceWithHirId<'tcx>, _diag_expr_id: hir::HirId) {}
        fn borrow(&mut self, _place_with_id: &PlaceWithHirId<'tcx>, _diag_expr_id: hir::HirId, _bk: ty::BorrowKind) {}
        fn mutate(&mut self, _assignee_place: &PlaceWithHirId<'tcx>, _diag_expr_id: hir::HirId) {}
        fn fake_read(&mut self, _place_with_id: &PlaceWithHirId<'tcx>, _cause: FakeReadCause, _diag_expr_id: hir::HirId) {}
    }

    // The expression use visitor is reached through a lint context, as clippy reaches it.
    let cx = LateContext {
        tcx,
        enclosing_body: Some(body_id),
        typeck_results: Some(typeck),
        param_env: tcx.param_env(body_owner),
        effective_visibilities: tcx.effective_visibilities(()),
        last_node_with_lint_attrs: tcx.local_def_id_to_hir_id(body_owner),
        generics: None,
        only_module: false,
    };
    let mut finder = OuterMoveFinder { tcx, loop_hir_id: loop_hir.hir_id, found: false };
    let Ok(()) = ExprUseVisitor::for_clippy(&cx, body_owner, &mut finder).walk_expr(loop_hir);
    finder.found
}

/// Swap continue expressions for break expressions and vice versa.
pub struct ContinueBreakSwap;

impl<'a> Operator<'a> for ContinueBreakSwap {
    type Mutation = ContinueBreakSwapMutation;

    fn try_apply(&self, mcx: &MutCtxt) -> Mutations<Self::Mutation> {
        let MutCtxt { opts: _, tcx, crate_res: _, def_res: _, def_site: def, item_hir: f_hir, body_res, location, value_is_borrowed: _ } = *mcx;

        let MutLoc::FnBodyExpr(expr, _) = location else { return Mutations::none(); };

        let swapped_expr = match &expr.kind {
            ast::ExprKind::Continue(label) => {
                ast::mk::expr(def, ast::ExprKind::Break(*label, None))
            }
            ast::ExprKind::Break(label, None) => {
                ast::mk::expr(def, ast::ExprKind::Continue(*label))
            }
            _ => { return Mutations::none(); }
        };

        let Some(body_hir) = f_hir.body else { return Mutations::none(); };
        let typeck = tcx.typeck_body(body_hir.id());

        let Some(expr_hir) = body_res.hir_expr(expr) else { unreachable!() };

        let (hir::ExprKind::Continue(destination) | hir::ExprKind::Break(destination, _)) = expr_hir.kind else { unreachable!() };
        let target_hir_id = destination.target_id.unwrap();
        // A `break` may leave a labeled block, which has nothing to `continue`.
        let hir::Node::Expr(target_hir @ hir::Expr { kind: hir::ExprKind::Loop(..), .. }) = tcx.hir_node(target_hir_id) else { return Mutations::none(); };

        let target_ty = typeck.node_type(target_hir_id);
        let compiles = match &expr.kind {
            // Only a `loop` without a `break` has type `!`, and a `continue` swapped for a `break` gives it
            // one, so it becomes `()` and can finish.
            ast::ExprKind::Continue(_) if target_ty == tcx.types.never => loop_may_become_unit(tcx, typeck, target_hir),
            ast::ExprKind::Break(..) if loop_moves_outer_value(tcx, typeck, f_hir.owner_id.def_id, body_hir.id(), target_hir) => false,
            _ => target_ty == tcx.types.unit || target_ty == tcx.types.never,
        };
        if !compiles { return Mutations::none(); }

        let mutation = Self::Mutation {
            original_expr: expr.kind.clone(),
            replacement_expr: swapped_expr.kind.clone(),
        };

        Mutations::new_one(mutation, smallvec![
            SubstDef::new(
                SubstLoc::Replace(expr.id, expr.span),
                Subst::AstExpr(*swapped_expr),
            ),
        ])
    }
}
