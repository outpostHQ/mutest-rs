use rustc_span::span_bug;

use crate::analysis::hir;
use crate::analysis::res;
use crate::codegen::ast;
use crate::codegen::symbols::{Span, kw};

use super::MacroExpansionSanitizer;

/// How many `super` segments a `super{::super}*` path prefix has, which may start with a `self` that changes nothing.
pub(super) fn super_count(segments: &[ast::PathSegment]) -> Option<usize> {
    let supers = match segments {
        [self_segment, supers @ ..] if self_segment.ident.name == kw::SelfLower => supers,
        _ => segments,
    };
    (!supers.is_empty() && supers.iter().all(|segment| segment.ident.name == kw::Super)).then_some(supers.len())
}

impl<'tcx, 'op> MacroExpansionSanitizer<'tcx, 'op> {
    pub(super) fn expect_visible_def_path(&self, request: res::DefPathRequestKind, ignore_reexport: Option<hir::DefId>, span: Span) -> res::DefPath<'tcx> {
        match res::visible_def_path(self.tcx, self.crate_res, request, self.current_scope, ignore_reexport, span) {
            Ok(def_path) => def_path,
            // NOTE: `TyCtxt::def_path_str` prints implicit tuple/unit variant constructors the same way as the variant itself
            //       (i.e., without `::{{constructor}}`), so no adjustments are needed.
            Err(None) => span_bug!(span, "`{}` is not accessible in this crate", self.tcx.def_path_str(request.def_id())),
            Err(Some(adjusted_scope)) => self.super_path_through_common_mod(request.def_id()).unwrap_or_else(|| {
                span_bug!(span, "`{def}` is not defined in {scope} and is not otherwise accessible here",
                    def = self.tcx.def_path_str(request.def_id()),
                    scope = match adjusted_scope.is_top_level_module() {
                        true => "the crate root".to_owned(),
                        false => format!("the scope `{}`", self.tcx.def_path_str(adjusted_scope)),
                    },
                );
            }),
        }
    }

    /// A `super` path to a local item in another branch of the module tree, through the nearest module containing both,
    /// for items that no path from the crate root reaches, such as those in modules inside function bodies.
    pub(super) fn super_path_through_common_mod(&self, def_id: hir::DefId) -> Option<res::DefPath<'tcx>> {
        let def_id = match self.tcx.def_kind(def_id) { hir::DefKind::Ctor(..) => self.tcx.parent(def_id), _ => def_id };
        let local_def_id = def_id.as_local()?;
        let current_scope = self.current_scope?;
        let containing_mod = self.tcx.parent_module_from_def_id(local_def_id).to_def_id();
        let mut common_mod = match self.tcx.def_kind(current_scope) {
            hir::DefKind::Mod => current_scope,
            _ => self.tcx.parent_module_from_def_id(current_scope.as_local()?).to_def_id(),
        };
        let mut supers = 0;
        while !self.tcx.is_descendant_of(containing_mod, common_mod) {
            common_mod = self.tcx.parent_module_from_def_id(common_mod.as_local()?).to_def_id();
            supers += 1;
        }
        let mut path = res::lexical_def_path(self.tcx, def_id, common_mod).ok()?;
        let [through @ .., _] = path.segments.as_slice() else { return None; };
        // NOTE: `super` never reaches into a function body, so the path may only pass through modules.
        let through_mods = through.iter().all(|segment| self.tcx.def_kind(segment.def_id) == hir::DefKind::Mod);
        if supers == 0 || !through_mods || !matches!(path.root, res::DefPathRootKind::Local) { return None; }
        path.root = res::DefPathRootKind::Parent { supers };
        Some(path)
    }
}
