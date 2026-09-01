use rustc_session::Session;

use crate::analysis::hir;
use crate::codegen::ast;
use crate::codegen::symbols::{DUMMY_SP, Ident, sym};

pub fn register(sess: &Session, krate: &mut ast::Crate) {
    let g = &sess.psess.attr_id_generator;

    // #![$name($arg)]
    let word_attr = |name, arg| ast::mk::attr_inner(g, DUMMY_SP,
        Ident::new(name, DUMMY_SP),
        ast::mk::attr_args_delimited(DUMMY_SP, ast::token::Delimiter::Parenthesis, ast::mk::token_stream(vec![
            ast::mk::tt_token_joint(DUMMY_SP, ast::TokenKind::Ident(arg, ast::token::IdentIsRaw::No)),
        ])),
    );

    krate.attrs.push(word_attr(sym::feature, sym::register_tool));
    krate.attrs.push(word_attr(sym::register_tool, sym::mutest));
    // Substitutions bind arm values with `super let` so their temporaries outlive the arm's scope.
    krate.attrs.push(word_attr(sym::feature, sym::super_let));
}

pub fn ignore<'tcx, I>(attrs: I) -> bool
where
    I: IntoIterator<Item = &'tcx hir::Attribute>,
{
    attrs.into_iter().any(|attr| hir::attr::is_word_attr(attr, Some(sym::mutest), sym::ignore))
}

pub fn skip<'tcx, I>(attrs: I) -> bool
where
    I: IntoIterator<Item = &'tcx hir::Attribute>,
{
    attrs.into_iter().any(|attr| hir::attr::is_word_attr(attr, Some(sym::mutest), sym::skip))
}
