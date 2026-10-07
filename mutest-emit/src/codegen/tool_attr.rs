use rustc_session::Session;

use crate::analysis::hir;
use crate::codegen::ast;
use crate::codegen::symbols::{DUMMY_SP, Ident, sym};

pub fn register(sess: &Session, krate: &mut ast::Crate) {
    let g = &sess.psess.attr_id_generator;

    // #![feature(register_tool)]
    let feature_register_tool_attr = ast::mk::attr_inner(g, DUMMY_SP,
        Ident::new(sym::feature, DUMMY_SP),
        ast::mk::attr_args_delimited(DUMMY_SP, ast::token::Delimiter::Parenthesis, ast::mk::token_stream(vec![
            ast::mk::tt_token_joint(DUMMY_SP, ast::TokenKind::Ident(sym::register_tool, ast::token::IdentKind::Normal)),
        ])),
    );
    // #![register_tool(mutest)]
    let register_tool_mutest_attr = ast::mk::attr_inner(g, DUMMY_SP,
        Ident::new(sym::register_tool, DUMMY_SP),
        ast::mk::attr_args_delimited(DUMMY_SP, ast::token::Delimiter::Parenthesis, ast::mk::token_stream(vec![
            ast::mk::tt_token_joint(DUMMY_SP, ast::TokenKind::Ident(sym::mutest, ast::token::IdentKind::Normal)),
        ])),
    );
    // #![feature(super_let)], used by the bindings of substitution arms.
    let feature_super_let_attr = ast::mk::attr_inner(g, DUMMY_SP,
        Ident::new(sym::feature, DUMMY_SP),
        ast::mk::attr_args_delimited(DUMMY_SP, ast::token::Delimiter::Parenthesis, ast::mk::token_stream(vec![
            ast::mk::tt_token_joint(DUMMY_SP, ast::TokenKind::Ident(sym::super_let, ast::token::IdentKind::Normal)),
        ])),
    );

    krate.attrs.push(feature_register_tool_attr);
    krate.attrs.push(register_tool_mutest_attr);
    krate.attrs.push(feature_super_let_attr);
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

/// Removes tool attributes, such as `#[clippy::format_args]`, from `#[macro_export]` macros. rustc defines such a macro
/// one expansion round late, so with no macro call left in the crate root, an import of the macro stays unresolved.
pub fn strip_from_exported_macros(krate: &mut ast::Crate) {
    struct ExportedMacroToolAttrStripper;

    impl ast::mut_visit::MutVisitor for ExportedMacroToolAttrStripper {
        fn visit_item(&mut self, item: &mut ast::Item) {
            if let ast::ItemKind::MacroDef(_, macro_def) = &item.kind && macro_def.macro_rules && ast::attr::contains_name(&item.attrs, sym::macro_export) {
                // Only tool attributes have paths with more than one segment.
                item.attrs.retain(|attr| attr.path().len() == 1);
            }
            ast::mut_visit::walk_item(self, item);
        }
    }

    ast::mut_visit::MutVisitor::visit_crate(&mut ExportedMacroToolAttrStripper, krate);
}
