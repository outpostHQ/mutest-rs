use mutest_emit::{Mutation, Operator};
use mutest_emit::analysis::ty;
use mutest_emit::codegen::ast;
use mutest_emit::codegen::mutation::{MutCtxt, MutLoc, Mutations, Subst, SubstDef, SubstLoc};
use rustc_data_structures::smallvec::{SmallVec, smallvec};

pub const STRUCT_FIELD_DELETE: &str = "struct_field_delete";

pub struct StructFieldDeleteMutation {
    pub field_ident: String,
}

impl Mutation for StructFieldDeleteMutation {
    fn op_name(&self) -> &str { STRUCT_FIELD_DELETE }

    fn display_name(&self) -> String {
        format!("take `{field}` from the base expression instead of the value given", field = self.field_ident)
    }

    fn span_label(&self) -> String {
        format!("take `{field}` from the base expression", field = self.field_ident)
    }
}

/// Delete fields from struct expressions with a base expression, so the base's values are used instead.
pub struct StructFieldDelete;

impl<'a> Operator<'a> for StructFieldDelete {
    type Mutation = StructFieldDeleteMutation;

    fn try_apply(&self, mcx: &MutCtxt) -> Mutations<Self::Mutation> {
        let MutCtxt { opts: _, tcx, crate_res: _, def_res: _, def_site: _, item_hir: f_hir, body_res, location, value_is_borrowed: _ } = *mcx;

        let MutLoc::FnBodyExpr(expr, _f) = location else { return Mutations::none(); };
        let ast::ExprKind::Struct(struct_expr) = &expr.kind else { return Mutations::none(); };
        let ast::StructRest::Base(base) = &struct_expr.rest else { return Mutations::none(); };

        let Some(body_hir) = f_hir.body else { return Mutations::none(); };
        let typeck = tcx.typeck_body(body_hir.id());
        let Some(base_hir) = body_res.hir_expr(base) else { return Mutations::none(); };
        let ty::Adt(base_def, base_args) = typeck.expr_ty(base_hir).kind() else { return Mutations::none(); };
        let Some(copy_trait) = tcx.lang_items().copy_trait() else { return Mutations::none(); };
        let typing_env = ty::TypingEnv::post_analysis(tcx, f_hir.owner_id.def_id);

        let mut mutations = SmallVec::new();
        for (i, field) in struct_expr.fields.iter().enumerate() {
            let Some(base_field) = base_def.non_enum_variant().fields.iter().find(|f| f.name == field.ident.name) else { continue; };
            let Some(field_hir) = body_res.hir_expr(&field.expr) else { continue; };
            let Ok(field_ty) = tcx.try_normalize_erasing_regions(typing_env, base_field.ty(tcx, base_args)) else { continue; };
            if field_ty != typeck.expr_ty_adjusted(field_hir) { continue; }
            // NOTE: Taking a non-`Copy` field moves it out of the base, which may still be used afterwards.
            if !ty::impls_trait(tcx, f_hir.owner_id.def_id, field_ty, copy_trait, vec![]) { continue; }

            let mut mutated_struct_expr = struct_expr.clone();
            mutated_struct_expr.fields = struct_expr.fields.iter().enumerate().filter(|&(j, _)| j != i).map(|(_, f)| f.clone()).collect();
            let mutated_expr = ast::mk::expr(expr.span, ast::ExprKind::Struct(mutated_struct_expr));

            let mutation = Self::Mutation { field_ident: field.ident.to_string() };
            mutations.push((mutation, smallvec![
                SubstDef::new(
                    SubstLoc::Replace(expr.id, expr.span),
                    Subst::AstExpr(*mutated_expr),
                ),
            ]));
        }

        Mutations::new(mutations)
    }
}
