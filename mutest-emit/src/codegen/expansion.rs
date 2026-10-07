use std::iter;

use itertools::Itertools;
use rustc_data_structures::fx::FxHashSet;
use rustc_data_structures::smallvec::{SmallVec, smallvec};
use rustc_data_structures::thin_vec::ThinVec;
use rustc_middle::ty::TyCtxt;
use rustc_session::Session;
use rustc_span::{ExpnData, LocalExpnId};
use rustc_span::edition::Edition;

use crate::analysis::tests::Test;
use crate::codegen::ast;
use crate::codegen::ast::mut_visit::MutVisitor;
use crate::codegen::symbols::{ExpnKind, Ident, Span, Symbol, sym};
use crate::codegen::symbols::hygiene::AstPass;

pub trait TcxExpansionExt {
    fn expansion_for_ast_pass(
        &self,
        ast_pass: AstPass,
        call_site: Span,
        features: &[Symbol],
    ) -> LocalExpnId;
}

impl<'tcx> TcxExpansionExt for TyCtxt<'tcx> {
    fn expansion_for_ast_pass(
        &self,
        ast_pass: AstPass,
        call_site: Span,
        features: &[Symbol],
    ) -> LocalExpnId {
        let expn_data = ExpnData::allow_unstable(
            ExpnKind::AstPass(ast_pass),
            call_site,
            self.sess.edition(),
            features.into(),
            None,
            None,
        );
        self.with_stable_hashing_context(|hcx| LocalExpnId::fresh(expn_data, hcx))
    }
}

pub const GENERATED_CODE_PRELUDE: &str = r#"
#![allow(unused_features)]
#![allow(unused_imports)]
"#;

/// Prints each block expression of a pre-2024 expansion in an edition 2024 crate as an edition 2021 block,
/// which keeps the temporaries of its tail expression alive until the end of the enclosing statement.
pub struct Edition2021BlockAnn {
    crate_edition: Edition,
    /// Closure bodies and anonymous constants, which do not accept a macro call in every position
    /// (e.g. `|| -> T { .. }`, `Foo<{ .. }>`), and whose block is already a terminating scope.
    unwrapped_exprs: FxHashSet<*const ast::Expr>,
}

struct UnwrappedExprCollector {
    unwrapped_exprs: FxHashSet<*const ast::Expr>,
}

impl<'ast> ast::visit::Visitor<'ast> for UnwrappedExprCollector {
    fn visit_expr(&mut self, expr: &'ast ast::Expr) {
        if let ast::ExprKind::Closure(closure) = &expr.kind {
            self.unwrapped_exprs.insert(&*closure.body);
        }
        ast::visit::walk_expr(self, expr);
    }

    fn visit_anon_const(&mut self, anon_const: &'ast ast::AnonConst) {
        self.unwrapped_exprs.insert(&*anon_const.value);
        ast::visit::walk_anon_const(self, anon_const);
    }
}

impl Edition2021BlockAnn {
    pub fn new(krate: &ast::Crate, crate_edition: Edition) -> Self {
        let mut collector = UnwrappedExprCollector { unwrapped_exprs: Default::default() };
        if crate_edition.at_least_rust_2024() {
            ast::visit::Visitor::visit_crate(&mut collector, krate);
        }
        Self { crate_edition, unwrapped_exprs: collector.unwrapped_exprs }
    }

    fn prints_edition_2021_block(&self, node: &ast::print::state::AnnNode<'_>) -> bool {
        if self.crate_edition.at_least_rust_2024()
            && let ast::print::state::AnnNode::Expr(expr) = node
            && let ast::ExprKind::Block(block, _) = &expr.kind
            && !block.span.at_least_rust_2024()
            && let Some(ast::Stmt { kind: ast::StmtKind::Expr(_), .. }) = block.stmts.last()
        {
            return !self.unwrapped_exprs.contains(&(&**expr as *const ast::Expr));
        }
        false
    }
}

impl ast::print::state::PpAnn for Edition2021BlockAnn {
    fn pre(&self, state: &mut ast::print::state::State<'_>, node: ast::print::state::AnnNode<'_>) {
        if !self.prints_edition_2021_block(&node) { return; }
        state.word("crate::mutest_generated::mutest_runtime::edition_2021_block! {");
        state.space();
    }

    fn post(&self, state: &mut ast::print::state::State<'_>, node: ast::print::state::AnnNode<'_>) {
        if !self.prints_edition_2021_block(&node) { return; }
        state.space();
        state.word("}");
    }
}

fn dedupe_extern_crate_decls(items: &mut ThinVec<Box<ast::Item>>, sym: Symbol) {
    if let Some((first_extern_crate_index, _)) = items.iter().find_position(|&item| ast::inspect::is_extern_crate_decl(item, sym)) {
        let mut i = first_extern_crate_index + 1;
        while let Some(item) = items.get(i) {
            if !ast::inspect::is_extern_crate_decl(item, sym) {
                i += 1;
                continue;
            }

            items.remove(i);
        }
    }
}

fn ensure_test_scope(items: &mut ThinVec<Box<ast::Item>>) {
    dedupe_extern_crate_decls(items, sym::test)
}

struct TestCaseCleaner<'tcx, 'tst> {
    sess: &'tcx Session,
    tests: &'tst [Test],
    /// Mark the test functions as tests again, rather than leaving them plain functions.
    keep_tests: bool,
}

impl<'tcx, 'tst> ast::mut_visit::MutVisitor for TestCaseCleaner<'tcx, 'tst> {
    fn visit_crate(&mut self, krate: &mut ast::Crate) {
        ast::mut_visit::walk_crate(self, krate);

        ensure_test_scope(&mut krate.items);
    }

    fn flat_map_item(&mut self, mut item: Box<ast::Item>) -> SmallVec<[Box<ast::Item>; 1]> {
        if let ast::ItemKind::Mod(..) = item.kind {
            ast::mut_visit::walk_item(self, &mut item);

            if let ast::ItemKind::Mod(_, _, ast::ModKind::Loaded(ref mut items, _, _)) = item.kind {
                ensure_test_scope(items);
            }
        }

        if let Some(_test) = self.tests.iter().find(|&test| test.descriptor.id == item.id) {
            return smallvec![];
        }

        if let Some(_test) = self.tests.iter().find(|&test| test.item.id == item.id) {
            let g = &self.sess.psess.attr_id_generator;

            let replacement_attr = match self.keep_tests {
                // #[test]
                true => ast::mk::attr_outer(g, item.span, ast::Safety::Default, Ident::new(sym::test, item.span), ast::AttrArgs::Empty),
                // #[allow(dead_code)]
                false => ast::mk::attr_outer(g, item.span, ast::Safety::Default, Ident::new(sym::allow, item.span),
                    ast::mk::attr_args_delimited(item.span, ast::token::Delimiter::Parenthesis, ast::mk::token_stream(vec![
                        ast::mk::tt_token_joint(item.span, ast::TokenKind::Ident(sym::dead_code, ast::token::IdentKind::Normal)),
                    ])),
                ),
            };

            item.attrs = item.attrs.into_iter()
                .filter(|attr| !attr.has_name(sym::rustc_test_marker))
                .filter(|attr| !attr.has_name(sym::test))
                .chain(iter::once(replacement_attr))
                .collect();
        }

        smallvec![item]
    }
}

pub fn clean_up_test_cases(sess: &Session, tests: &[Test], krate: &mut ast::Crate) {
    let mut cleaner = TestCaseCleaner { sess, tests, keep_tests: true };
    cleaner.visit_crate(krate);
}

/// Like `clean_up_test_cases`, but leaves the tests as plain functions, so the harness has none of them.
pub fn strip_test_cases(sess: &Session, tests: &[Test], krate: &mut ast::Crate) {
    let mut cleaner = TestCaseCleaner { sess, tests, keep_tests: false };
    cleaner.visit_crate(krate);
}
