//! Keeps the original code of a function body beside the body that holds its substitutions. One check at the
//! start of the body chooses between them, so a body with no active mutation runs no check at each location.

use rustc_data_structures::thin_vec::thin_vec;

use crate::codegen::ast;
use crate::codegen::ast::mut_visit::MutVisitor;
use crate::codegen::ast::visit::Visitor;
use crate::codegen::cancellation;
use crate::codegen::mutation::MutId;
use crate::codegen::symbols::{Ident, Span, Symbol, path, sym};

/// What a body holds that a second copy of the body would change the meaning of.
#[derive(Default)]
struct BodyShape {
    /// A closure or coroutine has a type of its own in each copy.
    closures: bool,
    /// An item other than an import would exist once in each copy.
    items: bool,
}

impl<'ast> Visitor<'ast> for BodyShape {
    fn visit_expr(&mut self, expr: &'ast ast::Expr) {
        self.closures |= matches!(expr.kind, ast::ExprKind::Closure(..) | ast::ExprKind::Gen(..));
        ast::visit::walk_expr(self, expr);
    }

    fn visit_item(&mut self, item: &'ast ast::Item) {
        self.items |= !matches!(item.kind, ast::ItemKind::Use(..) | ast::ItemKind::ExternCrate(..));
    }
}

struct OpaqueTyFinder(bool);

impl<'ast> Visitor<'ast> for OpaqueTyFinder {
    fn visit_ty(&mut self, ty: &'ast ast::Ty) {
        self.0 |= matches!(ty.kind, ast::TyKind::ImplTrait(..));
        ast::visit::walk_ty(self, ty);
    }
}

fn returns_opaque_ty(sig: &ast::FnSig) -> bool {
    let mut finder = OpaqueTyFinder(false);
    if let ast::FnRetTy::Ty(ty) = &sig.decl.output { finder.visit_ty(ty); }
    finder.0
}

/// Whether the function runs as plain code from its start to its end, unlike a constant or a coroutine.
fn runs_as_plain_code(constness: &ast::Const, coroutine_marker: &Option<ast::CoroutineMarker>) -> bool {
    matches!(constness, ast::Const::No) && coroutine_marker.is_none()
}

/// The body of the function to keep as its original code. A function has none if a copy of its body would hold
/// an item a second time, or would give an opaque return type the type of a second closure.
pub fn original_fn_body(func: &ast::Fn) -> Option<Box<ast::Block>> {
    let header = &func.sig.header;
    if !runs_as_plain_code(&header.constness, &header.coroutine_marker) || func.define_opaque.is_some() { return None; }
    let body = func.body.as_ref()?;
    let mut shape = BodyShape::default();
    shape.visit_block(body);
    let one_meaning = !shape.items && !(shape.closures && returns_opaque_ty(&func.sig));
    one_meaning.then(|| body.clone())
}

/// `_ if !$is_test_thread_active => $test_thread_cancel`
/// A test the run gave up must not go on into the code of another mutation, which would be undefined behavior.
pub fn mk_cancel_arm(sp: Span) -> ast::Arm {
    let test_thread_active_guard_expr = ast::mk::expr_not(sp, cancellation::mk_is_test_thread_active_expr(sp));
    ast::mk::arm(sp, ast::mk::pat_wild(sp), Some(test_thread_active_guard_expr), Some(cancellation::mk_test_thread_cancel_expr(sp)))
}

/// `unsafe { crate::mutest_generated::ACTIVE_MUTANT_HANDLE.subst_at_unchecked($slot) }`
pub fn mk_subst_lookup_expr(sp: Span, slot: usize) -> Box<ast::Expr> {
    ast::mk::expr_block(ast::mk::block_unsafe(sp, ast::UnsafeSource::CompilerGenerated, thin_vec![
        ast::mk::stmt_expr(ast::mk::expr_method_call_path_ident(sp, path::ACTIVE_MUTANT_HANDLE(sp), Ident::new(sym::subst_at_unchecked, sp), thin_vec![
            ast::mk::expr_lit(sp, ast::token::LitKind::Integer, Symbol::intern(&slot.to_string()), None),
        ])),
    ]))
}

/// The name of what a function body with a guard was entered with. The generated code is printed,
/// so the name alone keeps it apart from the names in the body.
fn entered_ident(sp: Span) -> Ident {
    Ident::new(Symbol::intern("mutest_entered"), sp)
}

/// `crate::mutest_generated::ACTIVE_MUTANT_HANDLE.runs_mutation_at($entered, $slot, $mut_id)`
pub fn mk_runs_mutation_expr(sp: Span, slot: usize, mut_id: MutId) -> Box<ast::Expr> {
    let lit = |value: String| ast::mk::expr_lit(sp, ast::token::LitKind::Integer, Symbol::intern(&value), None);
    let args = thin_vec![ast::mk::expr_ident(sp, entered_ident(sp)), lit(slot.to_string()), lit(mut_id.index().to_string())];
    ast::mk::expr_method_call_path_ident(sp, path::ACTIVE_MUTANT_HANDLE(sp), Ident::new(sym::runs_mutation_at, sp), args)
}

/// `match $lookup { _ if !$is_test_thread_active => $test_thread_cancel, None => $original, Some($entered) => $substituted }`
/// The slot is set for each mutation with a substitution in the body, so the original code runs for any other.
pub fn mk_guarded_body_expr(sp: Span, slot: usize, original: Box<ast::Block>, substituted: Box<ast::Block>) -> Box<ast::Expr> {
    let [original, substituted] = [original, substituted].map(|body| {
        let mut body = ast::mk::expr_block(body);
        LoopCancel { sp }.visit_expr(&mut body);
        body
    });
    ast::mk::expr_match(sp, mk_subst_lookup_expr(sp, slot), thin_vec![
        mk_cancel_arm(sp),
        ast::mk::arm(sp, ast::mk::pat_path(sp, path::None(sp)), None, Some(original)),
        ast::mk::arm(sp, ast::mk::pat_tuple_struct(sp, path::Some(sp), thin_vec![*ast::mk::pat_ident(sp, entered_ident(sp))]), None, Some(substituted)),
    ])
}

/// `match () { _ if !$is_test_thread_active => $test_thread_cancel, _ => {} };`
fn mk_cancel_stmt(sp: Span) -> ast::Stmt {
    let arms = thin_vec![mk_cancel_arm(sp), ast::mk::arm(sp, ast::mk::pat_wild(sp), None, Some(ast::mk::expr_noop(sp)))];
    ast::mk::stmt(sp, ast::StmtKind::Semi(ast::mk::expr_match(sp, ast::mk::expr_tuple(sp, thin_vec![]), arms)))
}

/// Makes each loop of a body cancel a test the run gave up, as a location in the body cancels it only where
/// it would run the code of another mutant. A closure stays as written, as no mutation is in a closure.
struct LoopCancel {
    sp: Span,
}

impl MutVisitor for LoopCancel {
    fn visit_expr(&mut self, expr: &mut ast::Expr) {
        if let ast::ExprKind::Closure(..) | ast::ExprKind::Gen(..) = &expr.kind { return; }
        ast::mut_visit::walk_expr(self, expr);
        if let Some(body) = loop_body(expr) { body.stmts.insert(0, mk_cancel_stmt(self.sp)); }
    }

    fn visit_anon_const(&mut self, _anon_const: &mut ast::AnonConst) {}
}

fn loop_body(expr: &mut ast::Expr) -> Option<&mut ast::Block> {
    match &mut expr.kind {
        ast::ExprKind::While(_, body, _) | ast::ExprKind::Loop(body, _, _) => Some(body),
        ast::ExprKind::ForLoop(for_loop) => Some(&mut for_loop.body),
        _ => None,
    }
}
