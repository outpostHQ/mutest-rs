use rustc_data_structures::fx::FxHashMap;
use rustc_data_structures::thin_vec::{ThinVec, thin_vec};
use rustc_middle::ty::TyCtxt;
use rustc_session::Session;

use crate::codegen::ast;
use crate::codegen::ast::mut_visit::MutVisitor;
use crate::codegen::expansion::TcxExpansionExt;
use crate::codegen::guard;
use crate::codegen::mutation::{Mut, MutId, Subst, SubstDef, SubstLoc};
use crate::codegen::symbols::{DUMMY_SP, Ident, Span, Symbol, path, sym};
use crate::codegen::symbols::hygiene::AstPass;

pub fn conflicting_substs(a: &SubstDef, b: &SubstDef) -> bool {
    match (&a.substitute, &b.substitute) {
        (Subst::AstLocal(..), Subst::AstLocal(..)) => false,
        _ => a.location == b.location,
    }
}

/// How a location reads the mutation to apply.
#[derive(Clone, Copy)]
pub enum SubstRead {
    /// From its own slot, where it also cancels a test the run gave up.
    Loc(usize),
    /// From the slot of the guard of its function body, and what the guard entered the body with.
    Guard(usize),
}

fn mk_subst_arm(sp: Span, read: SubstRead, mut_id: MutId, subst: Box<ast::Expr>) -> ast::Arm {
    // _ if $runs_mutation => $subst,
    if let SubstRead::Guard(slot) = read {
        return ast::mk::arm(sp, ast::mk::pat_wild(sp), Some(guard::mk_runs_mutation_expr(sp, slot, mut_id)), Some(subst));
    }

    // Some(mutest_subst) if mutest_subst.mutation.id == crate::mutest_generated::mutations::$mut_id.id => $subst,
    // The generated code is printed, so the name alone keeps the binding apart from the names in `$subst`.
    let subst_ident = Ident::new(Symbol::intern("mutest_subst"), sp);
    let pat_some_subst = ast::mk::pat_tuple_struct(sp, path::Some(sp), thin_vec![*ast::mk::pat_ident(sp, subst_ident)]);
    let guard = ast::mk::expr_binary(sp, ast::BinOpKind::Eq,
        ast::mk::expr_field_deep(sp, ast::mk::expr_ident(sp, subst_ident), vec![Ident::new(sym::mutation, sp), Ident::new(sym::id, sp)]),
        ast::mk::expr_field(sp, ast::mk::expr_path(ast::mk::pathx(sp, path::mutations(sp), vec![Ident::new(mut_id.into_symbol(), sp)])), Ident::new(sym::id, sp)),
    );
    ast::mk::arm(sp, pat_some_subst, Some(guard), Some(subst))
}

fn mk_subst_match_expr(sp: Span, read: SubstRead, default: Option<Box<ast::Expr>>, substs: Vec<(MutId, Box<ast::Expr>)>) -> Box<ast::Expr> {
    let (SubstRead::Loc(slot) | SubstRead::Guard(slot)) = read;
    let mut arm_idx = 0;
    let mut arms = substs.into_iter()
        .map(|(mut_id, subst)| mk_subst_arm(sp, read, mut_id, super_let_block(sp, slot, &mut arm_idx, subst)))
        .collect::<ThinVec<_>>();

    // _ => $default
    arms.push(ast::mk::arm(sp, ast::mk::pat_wild(sp), None, match default {
        Some(expr) => Some(super_let_block(sp, slot, &mut arm_idx, expr)),
        None => Some(ast::mk::expr_noop(sp)),
    }));

    let scrutinee = mk_subst_scrutinee_expr(sp, read, &mut arms);
    ast::mk::expr_paren(sp, ast::mk::expr_match(sp, scrutinee, arms))
}

/// The scrutinee of the match of a location. A location that reads its own slot gets the arm that cancels a test first.
fn mk_subst_scrutinee_expr(sp: Span, read: SubstRead, arms: &mut ThinVec<ast::Arm>) -> Box<ast::Expr> {
    match read {
        // match unsafe { crate::mutest_generated::ACTIVE_MUTANT_HANDLE.subst_at_unchecked($slot) } { ... }
        SubstRead::Loc(slot) => {
            arms.insert(0, guard::mk_cancel_arm(sp));
            guard::mk_subst_lookup_expr(sp, slot)
        }
        // match () { ... }
        SubstRead::Guard(_) => ast::mk::expr_tuple(sp, thin_vec![]),
    }
}

/// Extend borrowed temporaries beyond the match arm with `super let`.
/// Other expressions stay unbound to preserve their temporary scopes and coercions.
fn super_let_block(sp: Span, subst_loc_idx: usize, arm_idx: &mut usize, expr: Box<ast::Expr>) -> Box<ast::Expr> {
    if !matches!(expr.kind, ast::ExprKind::AddrOf(..)) { return expr; }

    let binding = Ident::new(Symbol::intern(&format!("subst_{subst_loc_idx}_{arm_idx}")), sp);
    *arm_idx += 1;

    ast::mk::expr_block(ast::mk::block(sp, thin_vec![
        ast::mk::stmt_super_let(sp, binding, expr),
        ast::mk::stmt_expr(ast::mk::expr_ident(sp, binding)),
    ]))
}

pub fn expand_subst_match_expr(sp: Span, read: SubstRead, original: Option<Box<ast::Expr>>, substs: Vec<(MutId, &Subst)>) -> Box<ast::Expr> {
    let subst_exprs = substs.into_iter()
        .map(|(mut_id, subst)| {
            let subst_expr = match subst {
                Subst::AstExpr(expr) => Box::new(expr.clone()),
                Subst::AstStmt(stmt) => ast::mk::expr_block(ast::mk::block(sp, thin_vec![stmt.clone()])),
                Subst::AstLocal(..) => panic!("invalid substitution: local substitutions cannot be made in expression positions"),
            };

            (mut_id, subst_expr)
        })
        .collect::<Vec<_>>();

    mk_subst_match_expr(sp, read, original, subst_exprs)
}

pub fn expand_subst_match_stmt(sp: Span, read: SubstRead, original: Option<ast::Stmt>, substs: Vec<(MutId, &Subst)>) -> Vec<ast::Stmt> {
    let mut binding_substs: Vec<(MutId, (Ident, ast::Mutability, Option<Box<ast::Ty>>, Box<ast::Expr>, Option<Box<ast::Expr>>))> = vec![];
    let mut non_binding_substs: Vec<(MutId, &Subst)> = vec![];

    for (mut_id, subst) in substs {
        match subst {
            Subst::AstLocal(ident, mutbl, ty, expr, default_expr) => {
                binding_substs.push((mut_id, (*ident, *mutbl, ty.clone(), expr.clone(), default_expr.clone())));
            }
            _ => non_binding_substs.push((mut_id, subst)),
        }
    }

    let mut stmts = Vec::with_capacity(binding_substs.len() + !non_binding_substs.is_empty() as usize);

    for (mut_id, (ident, mutbl, ty, expr, default_expr)) in binding_substs {
        // By default, a shadowing substitution is assumed, which can be reduced to identity by
        // assigning the value of the previous binding with the same identifier to the new binding
        // (and copying all of the properties of the original binding): `let $ident = $ident`.
        let default_expr = default_expr.unwrap_or_else(|| ast::mk::expr_ident(sp, ident));
        let subst_match_expr = mk_subst_match_expr(sp, read, Some(default_expr), vec![(mut_id, expr)]);

        let mutbl = matches!(mutbl, ast::Mutability::Mut);
        stmts.push(ast::mk::stmt_let(sp, mutbl, ident, ty, subst_match_expr));
    }

    if !non_binding_substs.is_empty() {
        let original_expr = original.map(|v| ast::mk::expr_block(ast::mk::block(sp, thin_vec![v])));
        stmts.push(ast::mk::stmt_expr(expand_subst_match_expr(sp, read, original_expr, non_binding_substs)));
    }

    stmts
}

/// What sets a slot of the substitution map of a mutant.
pub enum SubstSlot {
    /// A mutation with a substitution at the location.
    Loc(SubstLoc),
    /// One of the mutations, which are those with a substitution in one function body.
    Guard(Vec<MutId>),
}

/// The guard of a function body that keeps its original code, while the body is written.
struct BodyGuard {
    slot: usize,
    /// The mutations with a substitution at a location written so far.
    mut_ids: Vec<MutId>,
}

struct SubstWriter<'tcx, 'op> {
    sess: &'tcx Session,
    substitutions: FxHashMap<SubstLoc, Vec<(MutId, &'op Subst)>>,
    def_site: Span,
    slots: Vec<SubstSlot>,
    guard: Option<BodyGuard>,
}

impl<'tcx, 'op> SubstWriter<'tcx, 'op> {
    /// How the location with the substitutions reads the mutation to apply.
    fn read_at(&mut self, subst_loc: SubstLoc, substs: &[(MutId, &'op Subst)]) -> SubstRead {
        let Some(guard) = &mut self.guard else {
            self.slots.push(SubstSlot::Loc(subst_loc));
            return SubstRead::Loc(self.slots.len() - 1);
        };
        guard.mut_ids.extend(substs.iter().map(|(mut_id, _)| *mut_id));
        SubstRead::Guard(guard.slot)
    }

    fn visit_fn_item(&mut self, ctxt: ast::visit::FnCtxt, vis: &mut ast::Visibility, func: &mut ast::Fn) {
        let original = guard::original_fn_body(func);
        // A body that keeps its original code holds no function, so the guard of no outer body is in use here.
        self.guard = original.is_some().then(|| BodyGuard { slot: self.slots.len(), mut_ids: vec![] });
        ast::mut_visit::walk_fn(self, ast::mut_visit::FnKind::Fn(ctxt, vis, &mut *func));

        let (Some(original), Some(guard), Some(body)) = (original, self.guard.take(), &mut func.body) else { return; };
        if guard.mut_ids.is_empty() { return; }
        self.slots.push(SubstSlot::Guard(guard.mut_ids));

        let guarded_body = guard::mk_guarded_body_expr(self.def_site, guard.slot, original, body.clone());
        *body = ast::mk::block(self.def_site, thin_vec![ast::mk::stmt_expr(guarded_body)]);
    }
}

impl<'tcx, 'op> ast::mut_visit::MutVisitor for SubstWriter<'tcx, 'op> {
    fn visit_fn(&mut self, kind: ast::mut_visit::FnKind<'_>, _attrs: &ast::AttrVec, _sp: Span, _id: ast::NodeId) {
        match kind {
            ast::mut_visit::FnKind::Fn(ctxt, vis, func) => self.visit_fn_item(ctxt, vis, func),
            kind => ast::mut_visit::walk_fn(self, kind),
        }
    }

    fn visit_crate(&mut self, krate: &mut ast::Crate) {
        let g = &self.sess.psess.attr_id_generator;

        // #[allow(unused_parens)]
        let allow_unused_parens_attr = ast::mk::attr_inner(g, self.def_site,
            Ident::new(sym::allow, self.def_site),
            ast::mk::attr_args_delimited(self.def_site, ast::token::Delimiter::Parenthesis, ast::mk::token_stream(vec![
                ast::mk::tt_token_joint(self.def_site, ast::TokenKind::Ident(sym::unused_parens, ast::token::IdentKind::Normal)),
            ])),
        );

        krate.attrs.push(allow_unused_parens_attr);

        ast::mut_visit::walk_crate(self, krate);
    }

    fn visit_block(&mut self, block: &mut ast::Block) {
        ast::mut_visit::walk_block(self, block);

        let mut i = 0;
        while i < block.stmts.len() {
            let ast::Stmt { id: stmt_id, span: stmt_span, .. } = block.stmts[i];

            let insert_before_loc = SubstLoc::InsertBefore(stmt_id, stmt_span);
            if let Some(insertions_before) = self.substitutions.remove(&insert_before_loc) {
                let read = self.read_at(insert_before_loc, &insertions_before);

                let replacement_stmts = expand_subst_match_stmt(self.def_site, read, None, insertions_before);
                let replacement_stmts_count = replacement_stmts.len();

                block.stmts.splice(i..i, replacement_stmts);

                i += replacement_stmts_count;
            }

            let replacement_loc = SubstLoc::Replace(stmt_id, stmt_span);
            if let Some(replacements) = self.substitutions.remove(&replacement_loc) {
                let read = self.read_at(replacement_loc, &replacements);

                let replacement_stmts = expand_subst_match_stmt(self.def_site, read, None, replacements);
                let replacement_stmts_count = replacement_stmts.len();

                block.stmts.splice(i..i, replacement_stmts);

                i += replacement_stmts_count - 1;
            }

            let insert_after_loc = SubstLoc::InsertAfter(stmt_id, stmt_span);
            if let Some(insertions_after) = self.substitutions.remove(&insert_after_loc) {
                let read = self.read_at(insert_after_loc, &insertions_after);

                let replacement_stmts = expand_subst_match_stmt(self.def_site, read, None, insertions_after);
                let replacement_stmts_count = replacement_stmts.len();
                i += replacement_stmts_count;

                block.stmts.splice(i..i, replacement_stmts);
            }

            i += 1;
        }
    }

    fn visit_expr(&mut self, expr: &mut ast::Expr) {
        match &mut expr.kind {
            // The AST printer does not print the field name, only the expression, when using shorthand syntax. This
            // happens even if the field's expression is not an ident matching the field's name, resulting in malformed
            // code. To counter this, we force the printer to not use shorthand syntax when the field's expression is
            // substituted.
            ast::ExprKind::Struct(struct_expr) => {
                for field in &mut struct_expr.fields {
                    assert!(!self.substitutions.contains_key(&SubstLoc::Replace(field.id, field.span)), "field expression may not be mutated directly");
                    if self.substitutions.contains_key(&SubstLoc::Replace(field.expr.id, field.expr.span)) {
                        field.is_shorthand = false;
                    }
                }
            }
            _ => {}
        }

        ast::mut_visit::walk_expr(self, expr);

        if let Some(_insertions_before) = self.substitutions.remove(&SubstLoc::InsertBefore(expr.id, expr.span)) {
            panic!("invalid substitution: substitutions cannot be inserted before expressions");
        }

        let replacement_loc = SubstLoc::Replace(expr.id, expr.span);
        if let Some(replacements) = self.substitutions.remove(&replacement_loc) {
            let read = self.read_at(replacement_loc, &replacements);

            *expr = *expand_subst_match_expr(expr.span, read, Some(Box::new(expr.clone())), replacements);
        }

        if let Some(_insertions_after) = self.substitutions.remove(&SubstLoc::InsertAfter(expr.id, expr.span)) {
            panic!("invalid substitution: substitutions cannot be inserted after expressions");
        }
    }
}

pub fn write_substitutions<'tcx>(tcx: TyCtxt<'tcx>, mutations: &[Mut], krate: &mut ast::Crate) -> Vec<SubstSlot> {
    let mut substitutions: FxHashMap<SubstLoc, Vec<(MutId, &Subst)>> = Default::default();
    for mutation in mutations {
        for subst in &mutation.substs {
            let substitution = (mutation.id, &subst.substitute);

            substitutions.entry(subst.location)
                .and_modify(|substs| substs.push(substitution))
                .or_insert_with(|| vec![substitution]);
        }
    }

    let expn_id = tcx.expansion_for_ast_pass(
        AstPass::TestHarness,
        DUMMY_SP,
        &[sym::rustc_attrs, sym::super_let],
    );
    let def_site = DUMMY_SP.with_def_site_ctxt(expn_id.to_expn_id());

    let n_subst_locs = substitutions.len();

    // TODO: Warn if any substitutions have not been written to the AST. (e.g. they were defined for nodes which are not substitutable)
    let mut subst_writer = SubstWriter {
        sess: tcx.sess,
        substitutions,
        def_site,
        slots: Vec::with_capacity(n_subst_locs),
        guard: None,
    };
    subst_writer.visit_crate(krate);

    subst_writer.slots
}

struct SyntaxAmbiguityResolver<'tcx> {
    _sess: &'tcx Session,
    _def_site: Span,
}

impl<'tcx> ast::mut_visit::MutVisitor for SyntaxAmbiguityResolver<'tcx> {
    fn visit_expr(&mut self, expr: &mut ast::Expr) {
        ast::mut_visit::walk_expr(self, expr);

        match &expr.kind {
            // Expressions compared with a cast expression may be misinterpreted as type arguments for the type in the
            // cast expression. To avoid this, we simply parenthesize every cast expression.
            ast::ExprKind::Cast(_, _) => {
                *expr = *ast::mk::expr_paren(expr.span, Box::new(expr.clone()))
            }
            _ => {}
        }
    }
}

pub fn resolve_syntax_ambiguities<'tcx>(tcx: TyCtxt<'tcx>, krate: &mut ast::Crate) {
    let expn_id = tcx.expansion_for_ast_pass(
        AstPass::TestHarness,
        DUMMY_SP,
        &[sym::rustc_attrs],
    );
    let def_site = DUMMY_SP.with_def_site_ctxt(expn_id.to_expn_id());

    let mut syntax_ambiguity_resolver = SyntaxAmbiguityResolver { _sess: tcx.sess, _def_site: def_site };
    syntax_ambiguity_resolver.visit_crate(krate);
}
