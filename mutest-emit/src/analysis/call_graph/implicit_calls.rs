use std::iter;

use rustc_middle::mir;
use rustc_middle::ty::TyCtxt;

use crate::analysis::hir;
use crate::analysis::ty;
use crate::codegen::symbols::{DUMMY_SP, Span};

use super::{Call, CallKind};

pub fn drop_glue_callees<'tcx>(tcx: TyCtxt<'tcx>, body_mir: &'tcx mir::Body<'tcx>, generic_args: ty::GenericArgsRef<'tcx>) -> impl Iterator<Item = Call<'tcx>> {
    let instance = ty::Instance { def: body_mir.source.instance, args: generic_args };
    let typing_env = ty::TypingEnv::fully_monomorphized();

    body_mir.mentioned_items.iter().flatten()
        .filter_map(|mentioned_item| {
            match &mentioned_item.node {
                mir::MentionedItem::Drop(dropped_ty) => Some(dropped_ty),
                _ => None,
            }
        })
        .flat_map(move |&dropped_ty| {
            let dropped_ty = instance.instantiate_mir_and_normalize_erasing_regions(tcx, typing_env, ty::EarlyBinder::bind(tcx, dropped_ty));
            drop_glue_calls(tcx, dropped_ty, DUMMY_SP)
        })
}

fn drop_glue_calls<'tcx>(tcx: TyCtxt<'tcx>, dropped_ty: ty::Ty<'tcx>, span: Span) -> impl Iterator<Item = Call<'tcx>> {
    let drop_in_place = ty::Instance::resolve_drop_glue(tcx, dropped_ty);
    tcx.mir_inliner_callees(drop_in_place.def).iter()
        .map(move |&(def_id, generic_args)| {
            let safety = tcx.fn_sig(def_id).skip_binder().safety();
            Call { kind: CallKind::Def(def_id, generic_args), safety, span }
        })
}

fn def_safety(tcx: TyCtxt<'_>, def_id: hir::DefId) -> hir::Safety {
    match tcx.def_kind(def_id) {
        hir::DefKind::Closure => hir::Safety::Safe,
        _ => tcx.fn_sig(def_id).skip_binder().safety(),
    }
}

/// Finds the types that change in an unsizing coercion, e.g. `T` and `dyn Trait` in `Box<T>` to `Box<dyn Trait>`.
fn unsized_tails<'tcx>(source_ty: ty::Ty<'tcx>, target_ty: ty::Ty<'tcx>) -> (ty::Ty<'tcx>, ty::Ty<'tcx>) {
    match (source_ty.kind(), target_ty.kind()) {
        (&ty::Ref(_, source_ty, _) | &ty::RawPtr(source_ty, _), &ty::Ref(_, target_ty, _) | &ty::RawPtr(target_ty, _)) => unsized_tails(source_ty, target_ty),
        (&ty::Adt(source_adt, source_args), &ty::Adt(target_adt, target_args)) if source_adt == target_adt => {
            match iter::zip(source_args.types(), target_args.types()).find(|(source_ty, target_ty)| source_ty != target_ty) {
                Some((source_ty, target_ty)) => unsized_tails(source_ty, target_ty),
                None => (source_ty, target_ty),
            }
        }
        _ => (source_ty, target_ty),
    }
}

/// Returns the vtable methods and the drop glue that an unsizing coercion to a trait object makes callable.
fn vtable_calls<'tcx>(tcx: TyCtxt<'tcx>, source_ty: ty::Ty<'tcx>, target_ty: ty::Ty<'tcx>, span: Span) -> Vec<Call<'tcx>> {
    let (concrete_ty, dyn_ty) = unsized_tails(source_ty, target_ty);
    let &ty::Dynamic(predicates, ..) = dyn_ty.kind() else { return vec![]; };
    if concrete_ty.is_trait() { return vec![]; }

    let mut calls = drop_glue_calls(tcx, concrete_ty, span).collect::<Vec<_>>();
    if let Some(principal) = predicates.principal() {
        let trait_ref = tcx.instantiate_bound_regions_with_erased(principal.with_self_ty(tcx, concrete_ty));
        calls.extend(tcx.vtable_entries(trait_ref).iter().filter_map(|vtable_entry| {
            let ty::VtblEntry::Method(instance) = vtable_entry else { return None; };
            let safety = def_safety(tcx, instance.def_id());
            Some(Call { kind: CallKind::Def(instance.def_id(), instance.args), safety, span })
        }));
    }
    calls
}

/// Returns the functions that the body turns into function pointers or trait objects, as calls of the body.
// Based on `rustc_monomorphize::collector::MirUsedCollector::visit_rvalue`.
pub fn coercion_callees<'tcx>(tcx: TyCtxt<'tcx>, body_mir: &'tcx mir::Body<'tcx>, generic_args: ty::GenericArgsRef<'tcx>) -> impl Iterator<Item = Call<'tcx>> {
    let instance = ty::Instance { def: body_mir.source.instance, args: generic_args };
    let typing_env = ty::TypingEnv::fully_monomorphized();

    body_mir.basic_blocks.iter()
        .flat_map(|basic_block| &basic_block.statements)
        .filter_map(|statement| {
            let mir::StatementKind::Assign(assign) = &statement.kind else { return None; };
            let (_, mir::Rvalue::Cast(mir::CastKind::PointerCoercion(coercion, _), operand, target_ty)) = &**assign else { return None; };
            Some((statement.source_info.span, *coercion, operand, *target_ty))
        })
        .flat_map(move |(span, coercion, operand, target_ty)| {
            let source_ty = instance.instantiate_mir_and_normalize_erasing_regions(tcx, typing_env, ty::EarlyBinder::bind(tcx, operand.ty(&body_mir.local_decls, tcx)));
            match (coercion, source_ty.kind()) {
                (ty::adjustment::PointerCoercion::ReifyFnPointer(..), &ty::FnDef(def_id, generic_args)) => {
                    let generic_args = generic_args.no_bound_vars().unwrap();
                    vec![Call { kind: CallKind::Def(def_id, generic_args), safety: def_safety(tcx, def_id), span }]
                }
                (ty::adjustment::PointerCoercion::ClosureFnPointer(..), &ty::Closure(def_id, generic_args)) => {
                    vec![Call { kind: CallKind::Def(def_id, generic_args), safety: hir::Safety::Safe, span }]
                }
                (ty::adjustment::PointerCoercion::Unsize, _) => {
                    let target_ty = instance.instantiate_mir_and_normalize_erasing_regions(tcx, typing_env, ty::EarlyBinder::bind(tcx, target_ty));
                    vtable_calls(tcx, source_ty, target_ty, span)
                }
                _ => vec![],
            }
        })
}
