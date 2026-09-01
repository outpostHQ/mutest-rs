use mutest_emit::{Mutation, Operator};
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
        format!("take `{field}` from the base expression instead of the value given",
            field = self.field_ident,
        )
    }

    fn span_label(&self) -> String {
        format!("take `{field}` from the base expression", field = self.field_ident)
    }
}

/// Drop a field from a struct literal that has a base expression, so the base's value is used.
pub struct StructFieldDelete;

impl<'a> Operator<'a> for StructFieldDelete {
    type Mutation = StructFieldDeleteMutation;

    fn try_apply(&self, mcx: &MutCtxt) -> Mutations<Self::Mutation> {
        let MutCtxt { location, .. } = *mcx;

        let MutLoc::FnBodyExpr(expr, _) = location else { return Mutations::none(); };
        let ast::ExprKind::Struct(struct_expr) = &expr.kind else { return Mutations::none(); };

        // Without a base expression the field is required, and omitting it would not compile.
        let ast::StructRest::Base(_) = struct_expr.rest else { return Mutations::none(); };

        let mut mutations = SmallVec::new();
        for (i, field) in struct_expr.fields.iter().enumerate() {
            let mut mutated_struct = struct_expr.clone();
            mutated_struct.fields = struct_expr.fields.iter().enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, f)| f.clone())
                .collect();

            let mutated = ast::mk::expr(expr.span, ast::ExprKind::Struct(mutated_struct));
            let mutation = Self::Mutation { field_ident: field.ident.to_string() };
            mutations.push((mutation, smallvec![
                SubstDef::new(SubstLoc::Replace(expr.id, expr.span), Subst::AstExpr(*mutated)),
            ]));
        }

        Mutations::new(mutations)
    }
}
