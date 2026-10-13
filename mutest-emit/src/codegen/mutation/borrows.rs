use crate::analysis::hir;
use crate::analysis::ty::adjustment::{Adjust, AutoBorrow, AutoBorrowMutability};
use crate::codegen::ast;
use crate::codegen::ast::visit::Visitor;

use super::MutationCollector;

impl<'tcx, 'ast, 'op, 'trg, 'm> MutationCollector<'tcx, 'ast, 'op, 'trg, 'm> {
    /// Visit an expression the parent reads through, so it is not replaced by an owned temporary:
    /// temporary lifetime extension is syntactic, and a receiver or base is not an extending position.
    fn visit_borrowed_expr(&mut self, expr: &'ast ast::Expr) {
        self.value_is_borrowed = true;
        self.visit_expr(expr);
        self.value_is_borrowed = false;
    }

    /// Walk an expression, visiting the receiver or base it reads through as borrowed.
    pub(super) fn walk_expr_marking_borrows(&mut self, expr: &'ast ast::Expr) {
        match &expr.kind {
            ast::ExprKind::MethodCall(call) => {
                self.visit_borrowed_expr(&call.receiver);
                for arg in &call.args { self.visit_expr(arg); }
            }
            ast::ExprKind::Field(base, _) => self.visit_borrowed_expr(base),
            ast::ExprKind::Index(base, index, _) => {
                self.visit_borrowed_expr(base);
                self.visit_expr(index);
            }
            ast::ExprKind::AddrOf(_, _, inner) => self.visit_borrowed_expr(inner),
            _ => ast::visit::walk_expr(self, expr),
        }
    }

    /// Whether the parent can write through this place, so a substitution would put a temporary in its place,
    /// and e.g. `mem::take(&mut flag)` would reset the temporary, even with no mutation active.
    pub(super) fn is_mutably_borrowed_place(&self, body_hir: &hir::Body<'tcx>, expr_hir: &hir::Expr<'tcx>) -> bool {
        if !self.value_is_borrowed || !expr_hir.is_syntactic_place_expr() { return false; }
        match self.tcx.parent_hir_node(expr_hir.hir_id) {
            hir::Node::Expr(hir::Expr { kind: hir::ExprKind::AddrOf(_, mutability, _), .. }) => mutability.is_mut(),
            hir::Node::Expr(hir::Expr { kind: hir::ExprKind::MethodCall(_, receiver, _, _), .. }) if receiver.hir_id == expr_hir.hir_id => {
                let typeck = self.tcx.typeck_body(body_hir.id());
                typeck.expr_adjustments(expr_hir).iter().any(|adjustment| matches!(adjustment.kind,
                    Adjust::Borrow(AutoBorrow::Ref(AutoBorrowMutability::Mut { .. }) | AutoBorrow::RawPtr(hir::Mutability::Mut))
                ))
            }
            // NOTE: The parent projects a field or an element, which can be borrowed mutably in turn.
            _ => true,
        }
    }
}
