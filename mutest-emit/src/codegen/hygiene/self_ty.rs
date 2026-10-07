use crate::analysis::hir;
use crate::analysis::ty::{self, Ty};
use crate::codegen::ast;
use crate::codegen::symbols::Span;

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
pub(super) fn write_variant_args(path: &mut ast::Path, variant_args: Option<Box<ast::GenericArgs>>) {
    if let Some(variant_segment) = path.segments.last_mut() && variant_args.is_some() { variant_segment.args = variant_args; }
}

impl<'tcx, 'op> MacroExpansionSanitizer<'tcx, 'op> {
    /// Whether the self type of the path can keep its higher-ranked types inferred: in a body, with no generic args written for it.
    pub(super) fn infers_self_ty_args(&self, qself: &Option<Box<ast::QSelf>>, path: &ast::Path, qself_ty_hir_id: hir::HirId) -> bool {
        qself.is_none() && path.segments.iter().rev().skip(1).all(|segment| segment.args.is_none()) && self.is_inside_body(qself_ty_hir_id)
    }

    /// The enum generic args for the variant of a type-relative path with args written for its self type (e.g. `Alias::<T>::Variant`),
    /// which the sanitized path to the variant would otherwise lose with the self type.
    pub(super) fn enum_args_of_variant_path(&self, qself: &Option<Box<ast::QSelf>>, path: &ast::Path, qself_ty: Ty<'tcx>, node_hir_id: hir::HirId, span: Span) -> Option<Box<ast::GenericArgs>> {
        let ty::TyKind::Adt(adt_def, generic_args) = qself_ty.kind() else { return None; };
        let has_written_args = written_self_ty_segments(qself, path)?.iter().any(|segment| segment.args.is_some());
        if !adt_def.is_enum() || !has_written_args || path.segments.last()?.args.is_some() { return None; }
        self.sanitize_generic_args(adt_def.did(), generic_args, node_hir_id.owner.to_def_id(), self.is_inside_body(node_hir_id), span)
    }
}
