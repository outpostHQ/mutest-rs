use rustc_span::span_bug;

use crate::analysis::hir;
use crate::analysis::ty::{self, Ty};
use crate::codegen::ast;
use crate::codegen::symbols::{DUMMY_SP, Ident, Span};

use super::MacroExpansionSanitizer;

/// The path segments written for the self type of the type-relative path, if it is written as a path.
fn written_self_ty_segments<'a>(qself: &'a Option<Box<ast::QSelf>>, path: &'a ast::Path) -> Option<&'a [ast::PathSegment]> {
    match qself.as_deref().map(|qself| &qself.ty.kind) {
        None => path.segments.split_last().map(|(_, self_ty_segments)| self_ty_segments),
        Some(ast::TyKind::Path(None, self_ty_path)) => Some(&self_ty_path.segments),
        Some(_) => None,
    }
}

/// Writes the enum generic args from `enum_args_of_variant_path` on the variant segment of the sanitized path.
fn write_variant_args(path: &mut ast::Path, variant_args: Option<Box<ast::GenericArgs>>) {
    if let Some(variant_segment) = path.segments.last_mut() && variant_args.is_some() { variant_segment.args = variant_args; }
}

impl<'tcx, 'op> MacroExpansionSanitizer<'tcx, 'op> {
    /// Whether the self type of the path can keep its higher-ranked types inferred: in a body, with no generic args written for it.
    pub(super) fn infers_self_ty_args(&self, qself: &Option<Box<ast::QSelf>>, path: &ast::Path, qself_ty_hir_id: hir::HirId) -> bool {
        qself.is_none() && path.segments.iter().rev().skip(1).all(|segment| segment.args.is_none()) && self.is_inside_body(qself_ty_hir_id)
    }

    /// The enum generic args for the variant of a type-relative path with args written for its self type (e.g. `Alias::<T>::Variant`),
    /// which the sanitized path to the variant would otherwise lose with the self type.
    fn enum_args_of_variant_path(&self, qself: &Option<Box<ast::QSelf>>, path: &ast::Path, qself_ty: Ty<'tcx>, node_hir_id: hir::HirId, span: Span) -> Option<Box<ast::GenericArgs>> {
        let ty::TyKind::Adt(adt_def, generic_args) = qself_ty.kind() else { return None; };
        let has_written_args = written_self_ty_segments(qself, path)?.iter().any(|segment| segment.args.is_some());
        if !adt_def.is_enum() || !has_written_args || path.segments.last()?.args.is_some() { return None; }
        self.sanitize_generic_args(adt_def.did(), generic_args, node_hir_id.owner.to_def_id(), self.is_inside_body(node_hir_id), span)
    }

    /// Sanitizes the type-relative path to an item of an inherent impl into `<$ty>::$item`, with the type alias that the
    /// path names the self type by, or else with the self type from the type system.
    pub(super) fn sanitize_inherent_assoc_path(&mut self, qself: &mut Option<Box<ast::QSelf>>, path: &mut ast::Path, res: hir::Res<ast::NodeId>, qself_ty: Ty<'tcx>, qself_ty_hir: &hir::Ty<'tcx>, owner_def_id: hir::DefId) {
        let infer_higher_ranked_tys = self.infers_self_ty_args(qself, path, qself_ty_hir.hir_id);
        let opaque_ty_handling = ty::print::OpaqueTyHandling::Infer;
        let qself_ty_ast = self.written_alias_ty(qself, path, qself_ty_hir.hir_id)
            .or_else(|| ty::print::ty_ast(self.tcx, self.crate_res, self.def_res, self.current_scope, qself_ty_hir.span, qself_ty, opaque_ty_handling, infer_higher_ranked_tys, owner_def_id));
        let Some(qself_ty_ast) = qself_ty_ast else { span_bug!(qself_ty_hir.span, "cannot construct AST representation of type `{qself_ty:?}`") };

        // Discard self ty generic args in the original path.
        // NOTE: The parent path segment corresponds to the self ty, which we replace entirely after path sanitization.
        path.segments.iter_mut().rev().skip(1).for_each(|parent_path_segment| parent_path_segment.args = None);

        // NOTE: We always add a qualified self type, so we can safely ignore the indicator return value.
        let _ = self.sanitize_path(path, res, None);

        // Replace the non-assoc part of the path with the qualified self type, like so:
        // `<$ty>::$assoc` (note the lack of the `as` qualifier).
        path.segments.splice(0..(path.segments.len() - 1), []);
        *qself = Some(Box::new(ast::QSelf { ty: qself_ty_ast, path_span: DUMMY_SP, position: 0 }));
    }

    /// Sanitizes the type-relative path to an item that is not an associated item, such as a variant (e.g. `Alias::Variant`),
    /// into a path without a qualified self, through the type alias that names the self type if no path names the item here.
    pub(super) fn sanitize_non_assoc_path(&mut self, qself: &mut Option<Box<ast::QSelf>>, path: &mut ast::Path, res: hir::Res<ast::NodeId>, qself_ty: Ty<'tcx>, node_hir_id: hir::HirId, span: Span) {
        if !self.is_nameable_here(res.def_id()) && let Some((mut alias_path, _)) = self.written_alias_path(qself, path) {
            let Some(item_segment) = path.segments.last() else { unreachable!() };
            alias_path.segments.push(ast::PathSegment { ident: Ident::new(item_segment.ident.name, DUMMY_SP), ..item_segment.clone() });
            *path = alias_path;
        } else {
            let variant_args = self.enum_args_of_variant_path(qself, path, qself_ty, node_hir_id, span);
            let None = self.sanitize_path(path, res, None) else { span_bug!(path.span, "produced unexpected type-relative path for non-assoc path") };
            write_variant_args(path, variant_args);
        }
        *qself = None;
    }

    /// The sanitized path to the type alias that the type-relative path names its self type by (e.g. `dep::Alias` in
    /// `dep::Alias::Variant`), with the alias, if some path names the alias here.
    fn written_alias_path(&mut self, qself: &Option<Box<ast::QSelf>>, path: &ast::Path) -> Option<(ast::Path, hir::DefId)> {
        let alias_segments = written_self_ty_segments(qself, path)?;
        let alias_res @ hir::Res::Def(hir::DefKind::TyAlias, alias_def_id) = self.def_res.node_res(alias_segments.last()?.id)? else { return None; };
        if !self.is_nameable_here(alias_def_id) { return None; }
        let mut alias_path = ast::Path { span: path.span, segments: alias_segments.into() };
        let None = self.sanitize_path(&mut alias_path, alias_res, None) else { span_bug!(path.span, "produced unexpected type-relative path for type alias path") };
        Some((alias_path, alias_def_id))
    }

    /// The type alias that the type-relative path names its self type by, as a qualified self type, which keeps the generic
    /// args that the alias fixes even if no path names them here, with `_` for each arg that a path in a body leaves to inference.
    pub(super) fn written_alias_ty(&mut self, qself: &Option<Box<ast::QSelf>>, path: &ast::Path, qself_ty_hir_id: hir::HirId) -> Option<Box<ast::Ty>> {
        let (mut alias_path, alias_def_id) = self.written_alias_path(qself, path)?;
        if let Some(alias_segment) = alias_path.segments.last_mut() && alias_segment.args.is_none() && self.is_inside_body(qself_ty_hir_id) {
            let inferred_params = self.tcx.generics_of(alias_def_id).own_params.iter().filter(|param| param.kind.is_ty_or_const());
            alias_segment.args = ast::mk::angle_bracketed_args(DUMMY_SP, inferred_params.map(|_| ast::GenericArg::Type(ast::mk::ty(DUMMY_SP, ast::TyKind::Infer))).collect());
        }
        Some(ast::mk::ty_path(None, alias_path))
    }
}
