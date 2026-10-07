use std::cell::UnsafeCell;
use std::collections::hash_map;
use std::collections::vec_deque::VecDeque;
use std::hash::{Hash, Hasher};
use std::iter;
use std::mem;

use rustc_data_structures::fx::{FxHashSet, FxHashMap};
use rustc_middle::middle::codegen_fn_attrs::CodegenFnAttrFlags;
use rustc_middle::mir;
use rustc_middle::ty::TyCtxt;
use rustc_span::bug;

use crate::analysis::ast_lowering;
use crate::analysis::hir;
use crate::analysis::res;
use crate::analysis::tests::{self, Test, TestKind};
use crate::analysis::ty;
use crate::codegen::ast;
use crate::codegen::ast::visit::Visitor;
use crate::codegen::mutation::UnsafeTargeting;
use crate::codegen::symbols::{DUMMY_SP, Span, Symbol, span_diagnostic_ord, sym};
use crate::codegen::tool_attr;
use crate::stop;

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum UnsafeSource {
    EnclosingUnsafe,
    Unsafe,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Unsafety {
    None,
    /// Safe code called from an unsafe context.
    Tainted(UnsafeSource),
    Unsafe(UnsafeSource),
}

impl Unsafety {
    pub fn any(&self) -> bool {
        !matches!(self, Self::None)
    }

    pub fn is_unsafe(&self, unsafe_targeting: UnsafeTargeting) -> bool {
        matches!((unsafe_targeting, self),
            | (_, Unsafety::Unsafe(UnsafeSource::Unsafe) | Unsafety::Tainted(UnsafeSource::Unsafe))
            | (UnsafeTargeting::None, Unsafety::Unsafe(_) | Unsafety::Tainted(_))
            | (UnsafeTargeting::OnlyEnclosing(hir::Safety::Unsafe), Unsafety::Unsafe(UnsafeSource::EnclosingUnsafe) | Unsafety::Tainted(UnsafeSource::EnclosingUnsafe))
        )
    }
}

struct BodyUnsafetyChecker {
    unsafety: Option<Unsafety>,
}

impl<'ast> ast::visit::Visitor<'ast> for BodyUnsafetyChecker {
    fn visit_block(&mut self, block: &'ast ast::Block) {
        if let ast::BlockCheckMode::Unsafe(ast::UnsafeSource::UserProvided) = block.rules {
            self.unsafety = Some(Unsafety::Unsafe(UnsafeSource::EnclosingUnsafe));
            return;
        }

        ast::visit::walk_block(self, block);
    }
}

fn check_item_unsafety<'ast>(item: ast::DefItem<'ast>) -> Unsafety {
    let ast::DefItemKind::Fn(target_fn) = item.kind() else { return Unsafety::None };

    let (ast::Safety::Default | ast::Safety::Safe(_)) = target_fn.sig.header.safety else { return Unsafety::Unsafe(UnsafeSource::Unsafe) };

    let Some(target_body) = &target_fn.body else { return Unsafety::None };
    let mut checker = BodyUnsafetyChecker { unsafety: None };
    checker.visit_block(target_body);
    checker.unsafety.unwrap_or(Unsafety::None)
}

fn collect_unsafe_blocks<'tcx>(body_hir: &'tcx hir::Body<'tcx>, root_scope_safety: hir::Safety) -> Vec<&'tcx hir::Block<'tcx>> {
    struct BodyUnsafeBlockCollector<'tcx> {
        current_scope_safety: hir::Safety,
        unsafe_blocks: Vec<&'tcx hir::Block<'tcx>>,
    }

    impl<'tcx> hir::intravisit::Visitor<'tcx> for BodyUnsafeBlockCollector<'tcx> {
        fn visit_block(&mut self, block: &'tcx hir::Block<'tcx>) {
            let previous_scope_unsafety = self.current_scope_safety;
            self.current_scope_safety = match block.rules {
                | hir::BlockCheckMode::DefaultBlock
                // NOTE: We explicitly ignore compiler-generated unsafe blocks.
                | hir::BlockCheckMode::UnsafeBlock(hir::UnsafeSource::CompilerGenerated)
                => self.current_scope_safety,

                hir::BlockCheckMode::UnsafeBlock(hir::UnsafeSource::UserProvided) => hir::Safety::Unsafe,
            };

            if self.current_scope_safety == hir::Safety::Unsafe {
                self.unsafe_blocks.push(block);
            }

            hir::intravisit::walk_block(self, block);

            self.current_scope_safety = previous_scope_unsafety;
        }
    }

    let mut collector = BodyUnsafeBlockCollector {
        current_scope_safety: root_scope_safety,
        unsafe_blocks: vec![],
    };
    hir::intravisit::Visitor::visit_body(&mut collector, body_hir);
    collector.unsafe_blocks
}

#[derive(Copy, Clone, Debug)]
pub enum TargetKind {
    LocalMutable(hir::LocalDefId),
    ExternMutable(hir::DefId),
}

#[derive(Copy, Clone, Debug)]
pub enum TargetReachability {
    DirectEntry,
    NestedCallee { distance: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntryPointAssoc {
    pub distance: usize,
    pub unsafe_call_path: Option<UnsafeSource>,
}

#[derive(Copy, Clone, Debug)]
pub struct LocalEntryPoint {
    pub local_def_id: hir::LocalDefId,
    pub body_local_def_id: Option<hir::LocalDefId>,
}

impl LocalEntryPoint {
    pub fn body_local_def_id(&self) -> hir::LocalDefId {
        self.body_local_def_id.unwrap_or(self.local_def_id)
    }

    pub fn path_str<'tcx>(&self, tcx: TyCtxt<'tcx>) -> String {
        res::def_id_path(tcx, self.local_def_id.to_def_id()).iter()
            .skip(1) // Skip crate name in entry point path strings.
            .filter_map(|&segment_def_id| tcx.opt_item_name(segment_def_id).map(|symbol| symbol.as_str().to_owned()))
            .intersperse("::".to_owned())
            .collect::<String>()
    }
}

impl Eq for LocalEntryPoint {}
impl PartialEq for LocalEntryPoint {
    fn eq(&self, other: &Self) -> bool {
        self.local_def_id == other.local_def_id
    }
}

impl Hash for LocalEntryPoint {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.local_def_id.hash(state);
    }
}

#[derive(Copy, Clone, Debug)]
pub struct ExternEntryPoint {
    pub def_path_hash: hir::DefPathHash,
    pub path_str: Symbol,
}

impl Eq for ExternEntryPoint {}
impl PartialEq for ExternEntryPoint {
    fn eq(&self, other: &Self) -> bool {
        self.def_path_hash == other.def_path_hash
    }
}

impl Hash for ExternEntryPoint {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.def_path_hash.hash(state);
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum EntryPoint {
    Local(LocalEntryPoint),
    Extern(ExternEntryPoint),
}

impl EntryPoint {
    pub fn path_str<'tcx>(&self, tcx: TyCtxt<'tcx>) -> String {
        match self {
            EntryPoint::Local(local_entry_point) => local_entry_point.path_str(tcx),
            EntryPoint::Extern(extern_entry_point) => extern_entry_point.path_str.as_str().to_owned(),
        }
    }
}

#[derive(Debug)]
pub enum EntryPointAssocs {
    Local(FxHashMap<LocalEntryPoint, EntryPointAssoc>),
    Extern(FxHashMap<ExternEntryPoint, EntryPointAssoc>),
}

impl EntryPointAssocs {
    pub fn get(&self, entry_point: EntryPoint) -> Option<EntryPointAssoc> {
        match (&self, entry_point) {
            (EntryPointAssocs::Local(reachable_from), EntryPoint::Local(local_entry_point)) => reachable_from.get(&local_entry_point).copied(),
            (EntryPointAssocs::Local(_), _) => None,
            (EntryPointAssocs::Extern(reachable_from), EntryPoint::Extern(extern_entry_point)) => reachable_from.get(&extern_entry_point).copied(),
            (EntryPointAssocs::Extern(_), _) => None,
        }
    }

    pub fn iter(&self) -> Box<dyn Iterator<Item = (EntryPoint, EntryPointAssoc)> + '_> {
        match self {
            EntryPointAssocs::Local(reachable_from) => {
                let iter = reachable_from.iter()
                    .map(|(&local_entry_point, &entry_point_assoc)| (EntryPoint::Local(local_entry_point), entry_point_assoc));
                Box::new(iter)
            }
            EntryPointAssocs::Extern(reachable_from) => {
                let iter = reachable_from.iter()
                    .map(|(&extern_entry_point, &entry_point_assoc)| (EntryPoint::Extern(extern_entry_point), entry_point_assoc));
                Box::new(iter)
            }
        }
    }
}

#[derive(Debug)]
pub struct Target {
    pub kind: TargetKind,
    pub unsafety: Unsafety,
    pub reachability: TargetReachability,
    pub reachable_from: EntryPointAssocs,
}

impl Target {
    pub fn def_id(&self) -> hir::DefId {
        match self.kind {
            TargetKind::LocalMutable(local_def_id) => local_def_id.to_def_id(),
            TargetKind::ExternMutable(def_id) => def_id,
        }
    }

    pub fn is_tainted(&self, entry_point: EntryPoint, unsafe_targeting: UnsafeTargeting) -> bool {
        self.reachable_from.get(entry_point).is_some_and(|entry_point_assoc| {
            let unsafety = entry_point_assoc.unsafe_call_path.map(Unsafety::Tainted).unwrap_or(Unsafety::None);
            unsafety.is_unsafe(unsafe_targeting)
        })
    }
}

#[derive(Copy, Clone, Debug)]
pub enum Targeting {
    LocalMutables,
    ExternMutables(hir::CrateNum),
}

impl Targeting {
    pub fn matches<'tcx>(&self, tcx: TyCtxt<'tcx>, test_def_ids: &FxHashSet<hir::LocalDefId>, def_id: hir::DefId) -> Option<TargetKind> {
        match *self {
            Targeting::LocalMutables => {
                let Some(local_def_id) = def_id.as_local() else { return None; };
                let hir_id = tcx.local_def_id_to_hir_id(local_def_id);

                let entry_fn = tcx.entry_fn(());

                // TODO: Ignore #[coverage(off)] functions
                // Functions, excluding closures
                let targeted = matches!(tcx.def_kind(def_id), hir::DefKind::Fn | hir::DefKind::AssocFn)
                    // NOT `fn main() {}`
                    && !entry_fn.map(|(entry_def_id, _)| def_id == entry_def_id).unwrap_or(false)
                    // NOT `const fn`
                    && !tcx.is_const_fn(def_id)
                    // NOT `fn;`
                    && !tcx.hir_node_by_def_id(local_def_id).body_id().is_none()
                    // NOT `#[test]` functions, or inner functions
                    && !test_def_ids.contains(&local_def_id)
                    && !res::parent_iter(tcx, def_id).any(|parent_id| parent_id.as_local().is_some_and(|local_parent_id| test_def_ids.contains(&local_parent_id)))
                    // NOT `#[cfg(test)]` functions, or functions in `#[cfg(test)]` modules
                    && !tests::is_marked_or_in_cfg_test(tcx, hir_id)
                    // NOT `#[mutest::skip]` functions
                    && !tool_attr::skip(tcx.hir_attrs(hir_id));

                if !targeted { return None; }

                Some(TargetKind::LocalMutable(local_def_id))
            }
            Targeting::ExternMutables(cnum) => {
                // NOTE: This is in the context of the local crate invoking a non-test extern crate,
                //       so we can assume that
                //       the definition is not a `#[test]` function, a #[cfg(test)] function, or
                //       a function in a `#[cfg(test)]` module.

                let targeted = def_id.krate == cnum
                    // Functions, excluding closures
                    && matches!(tcx.def_kind(def_id), hir::DefKind::Fn | hir::DefKind::AssocFn)
                    // NOT `const fn`
                    && !tcx.is_const_fn(def_id)
                    // NOT `fn;`
                    && tcx.is_mir_available(def_id)
                    // NOT `#[mutest::skip]` functions
                    && !tool_attr::skip(hir::attrs::HasAttrs::get_attrs(def_id, &tcx));

                if !targeted { return None; }

                Some(TargetKind::ExternMutable(def_id))
            }
        }
    }

    /// Returns whether a call from the definition adds to the distance of the callee. Only frames of the local and the
    /// targeted crate count, so a test reaches a function at the same distance through any depth of library code.
    pub fn counts_call_frame(&self, def_id: hir::DefId) -> bool {
        match *self {
            Targeting::LocalMutables => def_id.is_local(),
            Targeting::ExternMutables(cnum) => def_id.is_local() || def_id.krate == cnum,
        }
    }
}

/// All functions we can introduce mutations in.
/// Does not include closures, as they are (currently) considered part of their containing function, rather than
/// standalone functions. This might change in the future.
pub fn all_mutable_fns<'tcx, 'tst>(tcx: TyCtxt<'tcx>, cnum: Option<hir::CrateNum>, tests: &'tst [Test]) -> Box<dyn Iterator<Item = hir::DefId> + 'tcx> {
    match cnum {
        None => {
            let test_def_ids = tests.iter().map(|test| test.def_id).collect::<FxHashSet<_>>();

            let iter = tcx.hir_crate_items(()).definitions().filter(move |&local_def_id| {
                Targeting::LocalMutables.matches(tcx, &test_def_ids, local_def_id.to_def_id()).is_some()
            });

            Box::new(iter.map(|local_def_id| local_def_id.to_def_id()))
        }
        Some(cnum) => {
            const BASE_DEF_IDX: usize = hir::CRATE_DEF_INDEX.as_usize();
            let max_def_idx = tcx.num_extern_def_ids(cnum);
            let crate_def_ids = (BASE_DEF_IDX..max_def_idx).map(move |def_idx| hir::DefId { krate: cnum, index: hir::DefIndex::from_usize(def_idx) });

            let iter = crate_def_ids.filter(move |&def_id| {
                Targeting::ExternMutables(cnum).matches(tcx, &Default::default(), def_id).is_some()
            });

            Box::new(iter)
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum EntryPoints<'a> {
    Tests(&'a [Test]),
    External,
}

impl<'a> EntryPoints<'a> {
    pub fn iter(&self) -> Box<dyn Iterator<Item = LocalEntryPoint> + 'a> {
        match self {
            EntryPoints::Tests(tests) => {
                let iter = tests.iter()
                    .filter(|test| !test.ignore)
                    .map(|test| match &test.kind {
                        &TestKind::Test => LocalEntryPoint { local_def_id: test.def_id, body_local_def_id: None },
                        &TestKind::QuickCheck { original_def_id } => LocalEntryPoint { local_def_id: test.def_id, body_local_def_id: Some(original_def_id) },
                    });
                Box::new(iter)
            },
            EntryPoints::External => Box::new(iter::empty()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CallKind<'tcx> {
    Def(hir::DefId, ty::GenericArgsRef<'tcx>),
    Ptr(ty::PolyFnSig<'tcx>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Call<'tcx> {
    pub kind: CallKind<'tcx>,
    pub safety: hir::Safety,
    pub span: Span,
}

// Based on `rustc_mir_transform::inline::cycle::mir_inliner_callees`.
pub fn mir_callees<'tcx>(tcx: TyCtxt<'tcx>, body_mir: &'tcx mir::Body<'tcx>, generic_args: ty::GenericArgsRef<'tcx>) -> impl Iterator<Item = Call<'tcx>> {
    let instance = ty::Instance { def: body_mir.source.instance, args: generic_args };
    let typing_env = ty::TypingEnv::fully_monomorphized();

    let body_def_id = body_mir.source.instance.def_id();
    let body_safety = match tcx.def_kind(body_def_id) {
        hir::DefKind::Closure => hir::Safety::Safe,
        _ => tcx.fn_sig(body_def_id).skip_binder().safety(),
    };

    let body_hir = body_def_id.as_local()
        .and_then(|local_def_id| tcx.hir_node_by_def_id(local_def_id).body_id())
        .map(|body_id| tcx.hir_body(body_id));
    let unsafe_blocks = body_hir.map(|body_hir| collect_unsafe_blocks(body_hir, body_safety));

    body_mir.basic_blocks.iter()
        .filter_map(move |basic_block| {
            let terminator = basic_block.terminator();
            let mir::TerminatorKind::Call { func, args: call_args, .. } = &terminator.kind else { return None; };

            let ty = func.ty(&body_mir.local_decls, tcx);
            let span = terminator.source_info.span;

            let mut safety = body_safety;

            if safety != hir::Safety::Unsafe {
                if let Some(unsafe_blocks) = &unsafe_blocks {
                    if unsafe_blocks.iter().any(|unsafe_block| span.find_ancestor_inside_same_ctxt(unsafe_block.span).is_some()) {
                        safety = hir::Safety::Unsafe;
                    }
                }
            }

            match ty.kind() {
                &ty::TyKind::FnDef(mut def_id, mut generic_args) => {
                    if tcx.is_intrinsic(def_id, sym::const_eval_select) {
                        let func = &call_args[2].node;
                        let ty = func.ty(&body_mir.local_decls, tcx);
                        let &ty::TyKind::FnDef(inner_def_id, inner_generic_args) = ty.kind() else { return None; };

                        def_id = inner_def_id;
                        generic_args = inner_generic_args;
                    }

                    // See https://github.com/rust-lang/rust/pull/158632.
                    let generic_args = generic_args.no_bound_vars().unwrap();

                    let generic_args = instance.instantiate_mir_and_normalize_erasing_regions(tcx, typing_env, ty::EarlyBinder::bind(tcx, generic_args));

                    if safety != hir::Safety::Unsafe {
                        safety = tcx.fn_sig(def_id).skip_binder().safety();
                    }

                    Some(Call { kind: CallKind::Def(def_id, generic_args), safety, span })
                }

                &ty::TyKind::FnPtr(fn_sig_tys, fn_header) => {
                    let fn_sig_tys = instance.instantiate_mir_and_normalize_erasing_regions(tcx, typing_env, ty::EarlyBinder::bind(tcx, fn_sig_tys));

                    if safety != hir::Safety::Unsafe {
                        safety = fn_header.safety();
                    }

                    Some(Call { kind: CallKind::Ptr(fn_sig_tys.with(fn_header)), safety, span })
                }

                _ => None,
            }
        })
}

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
        .map(move |&dropped_ty| {
            let dropped_ty = instance.instantiate_mir_and_normalize_erasing_regions(tcx, typing_env, ty::EarlyBinder::bind(tcx, dropped_ty));
            ty::Instance::resolve_drop_glue(tcx, dropped_ty)
        })
        .flat_map(move |drop_in_place| tcx.mir_inliner_callees(drop_in_place.def))
        .map(move |&(def_id, generic_args)| {
            let safety = tcx.fn_sig(def_id).skip_binder().safety();
            Call { kind: CallKind::Def(def_id, generic_args), safety, span: DUMMY_SP }
        })
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct Callee<'tcx> {
    pub def_id: hir::DefId,
    pub generic_args: ty::GenericArgsRef<'tcx>,
}

impl<'tcx> Callee<'tcx> {
    pub fn new(def_id: hir::DefId, generic_args: ty::GenericArgsRef<'tcx>) -> Self {
        Self { def_id, generic_args }
    }

    pub fn display_str(&self, tcx: TyCtxt<'tcx>) -> String {
        tcx.def_path_str_with_args(self.def_id, self.generic_args)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InstanceCall<'tcx> {
    pub callee: Callee<'tcx>,
    pub safety: hir::Safety,
    pub span: Span,
}

pub struct CallGraph<'tcx> {
    pub virtual_calls_count: usize,
    pub dynamic_calls_count: usize,
    pub foreign_calls_count: usize,
    pub root_calls: FxHashMap<hir::LocalDefId, Vec<InstanceCall<'tcx>>>,
    pub nested_calls: Vec<FxHashMap<Callee<'tcx>, Vec<InstanceCall<'tcx>>>>,
}

impl<'tcx> CallGraph<'tcx> {
    pub fn total_calls_count(&self) -> usize {
        let mut total_calls_count = self.root_calls.iter().map(|(_, calls)| calls.len()).sum::<usize>()
            + self.nested_calls.iter().map(|calls| calls.iter().map(|(_, calls)| calls.len()).sum::<usize>()).sum::<usize>();
        // NOTE: Dynamic calls are currently not represented in the call graph, therefore
        //       we have to add their count manually to the total.
        total_calls_count += self.dynamic_calls_count;

        total_calls_count
    }

    pub fn depth(&self) -> usize {
        (!self.root_calls.is_empty() as usize) + self.nested_calls.len()
    }

    pub fn callees_of_nested_caller(&self, caller: Callee<'tcx>) -> Option<&[InstanceCall<'tcx>]> {
        self.nested_calls.iter().find_map(|calls| calls.get(&caller).map(|v| &**v))
    }
}

pub fn instantiate_generic_args<'tcx, T>(tcx: TyCtxt<'tcx>, foldable: T, generic_args: ty::GenericArgsRef<'tcx>) -> T
where
    T: ty::TypeFoldable<TyCtxt<'tcx>>,
{
    ty::EarlyBinder::bind(tcx, foldable).instantiate(tcx, generic_args).skip_normalization()
}

fn new_nested_target<'ast, 'tcx>(
    tcx: TyCtxt<'tcx>,
    def_res: &ast_lowering::DefResolutions,
    krate: &'ast ast::Crate,
    test_def_ids: &FxHashSet<hir::LocalDefId>,
    targeting: Targeting,
    def_id: hir::DefId,
    distance: usize,
) -> Option<Target> {
    let target_kind = targeting.matches(tcx, test_def_ids, def_id)?;

    let item_unsafety = match def_id.as_local() {
        Some(local_def_id) => check_item_unsafety(ast_lowering::find_def_in_ast(tcx, def_res, local_def_id, krate)?),
        None => {
            match tcx.fn_sig(def_id).skip_binder().safety() {
                hir::Safety::Safe => Unsafety::None,
                hir::Safety::Unsafe => Unsafety::Unsafe(UnsafeSource::Unsafe),
            }
        }
    };

    Some(Target {
        kind: target_kind,
        unsafety: item_unsafety,
        reachability: TargetReachability::NestedCallee { distance },
        reachable_from: EntryPointAssocs::Local(Default::default()),
    })
}

fn record_target<'ast, 'tcx>(
    tcx: TyCtxt<'tcx>,
    def_res: &ast_lowering::DefResolutions,
    krate: &'ast ast::Crate,
    test_def_ids: &FxHashSet<hir::LocalDefId>,
    entry_point: LocalEntryPoint,
    targeting: Targeting,
    caller: Callee<'tcx>,
    unsafety: Option<UnsafeSource>,
    distance: usize,
    targets: &mut FxHashMap<hir::DefId, Target>,
) {
    let target = match targets.entry(caller.def_id) {
        hash_map::Entry::Occupied(entry) => entry.into_mut(),
        hash_map::Entry::Vacant(entry) => {
            let Some(target) = new_nested_target(tcx, def_res, krate, test_def_ids, targeting, caller.def_id, distance) else { return; };
            entry.insert(target)
        }
    };

    if let TargetReachability::NestedCallee { distance: target_distance } = &mut target.reachability {
        *target_distance = Ord::min(distance, *target_distance)
    }

    let caller_tainting = unsafety.map(Unsafety::Tainted).unwrap_or(Unsafety::None);
    target.unsafety = Ord::max(caller_tainting, target.unsafety);

    let EntryPointAssocs::Local(reachable_from) = &mut target.reachable_from else { unreachable!() };
    let entry_point = reachable_from.entry(entry_point).or_insert_with(|| {
        EntryPointAssoc {
            distance,
            unsafe_call_path: None,
        }
    });

    entry_point.distance = Ord::min(distance, entry_point.distance);
    entry_point.unsafe_call_path = Ord::max(unsafety, entry_point.unsafe_call_path);
}

/// Records a reach of the callee, and returns whether it is closer or more unsafe than all earlier reaches.
fn reaches_closer_or_more_unsafely<'tcx>(reached: &mut FxHashMap<Callee<'tcx>, (usize, Option<UnsafeSource>)>, callee: Callee<'tcx>, distance: usize, unsafety: Option<UnsafeSource>) -> bool {
    match reached.entry(callee) {
        hash_map::Entry::Occupied(mut entry) => {
            let (min_distance, max_unsafety) = entry.get_mut();
            if distance >= *min_distance && unsafety <= *max_unsafety { return false; }
            *min_distance = Ord::min(distance, *min_distance);
            *max_unsafety = Ord::max(unsafety, *max_unsafety);
            true
        }
        hash_map::Entry::Vacant(entry) => {
            entry.insert((distance, unsafety));
            true
        }
    }
}

/// Returns whether the definition is a foreign item to report: not an intrinsic, an allocator function, or a
/// `#[rustc_std_internal_symbol]` function of the internal mechanisms of the standard library.
fn is_reported_foreign_item(tcx: TyCtxt<'_>, def_id: hir::DefId) -> bool {
    if !tcx.is_foreign_item(def_id) || tcx.intrinsic(def_id).is_some() { return false; }

    !tcx.codegen_fn_attrs(def_id).flags.intersects(
        CodegenFnAttrFlags::ALLOCATOR
        | CodegenFnAttrFlags::DEALLOCATOR
        | CodegenFnAttrFlags::REALLOCATOR
        | CodegenFnAttrFlags::ALLOCATOR_ZEROED
        | CodegenFnAttrFlags::RUSTC_STD_INTERNAL_SYMBOL
    )
}

/// Resolves the callee instance of the call, and counts and reports virtual, dynamic and foreign calls.
fn resolve_callee<'tcx>(tcx: TyCtxt<'tcx>, call_graph: &mut CallGraph<'tcx>, caller: Callee<'tcx>, call: Call<'tcx>) -> Option<Callee<'tcx>> {
    // NOTE: We are post type-checking, querying monomorphic obligations.
    let typing_env = ty::TypingEnv::fully_monomorphized();

    match call.kind {
        CallKind::Def(def_id, generic_args) => {
            // The type arguments from the local, generic scope may still contain type parameters, so we
            // fold the bound type arguments of the concrete invocation of the enclosing function into it.
            let generic_args = instantiate_generic_args(tcx, generic_args, caller.generic_args);
            // We resolve the definition instance with the concrete type arguments of this call. The type arguments
            // might take a different form at the resolved definition site, so we propagate them instead.
            let instance = ty::Instance::expect_resolve(tcx, typing_env, def_id, generic_args, DUMMY_SP);

            if let ty::InstanceKind::Virtual(def_id, _) = instance.def {
                call_graph.virtual_calls_count += 1;

                let mut diagnostic = tcx.dcx().struct_warn("encountered virtual call during call graph construction");
                diagnostic.span(call.span);
                diagnostic.span_label(call.span, format!("call to {}", tcx.def_path_str_with_args(def_id, instance.args)));
                diagnostic.note(format!("in {}", tcx.def_path_str_with_args(caller.def_id, caller.generic_args)));
                diagnostic.emit();
            }

            if is_reported_foreign_item(tcx, instance.def_id()) {
                call_graph.foreign_calls_count += 1;

                let mut diagnostic = tcx.dcx().struct_warn("encountered foreign call during call graph construction");
                diagnostic.span(call.span);
                diagnostic.span_label(call.span, format!("call to {}", tcx.def_path_str_with_args(instance.def_id(), instance.args)));
                diagnostic.note(format!("in {}", tcx.def_path_str_with_args(caller.def_id, caller.generic_args)));
                diagnostic.emit();
            }

            Some(Callee::new(instance.def_id(), instance.args))
        }

        CallKind::Ptr(fn_sig) => {
            call_graph.dynamic_calls_count += 1;

            let mut diagnostic = tcx.dcx().struct_warn("encountered dynamic call during call graph construction");
            diagnostic.span(call.span);
            diagnostic.span_label(call.span, format!("call to {fn_sig}"));
            diagnostic.note(format!("in {}", tcx.def_path_str_with_args(caller.def_id, caller.generic_args)));
            diagnostic.emit();

            None
        }
    }
}

fn sort_callers_by_span<'tcx>(tcx: TyCtxt<'tcx>, callers: FxHashSet<Callee<'tcx>>) -> Vec<Callee<'tcx>> {
    let mut callers = callers.into_iter().collect::<Vec<_>>();
    // HACK: We must sort the callers into a stable order for the corresponding diagnostics to be printed in a stable order.
    callers.sort_unstable_by(|caller_a, caller_b| {
        let caller_a_span = tcx.def_span(caller_a.def_id);
        let caller_b_span = tcx.def_span(caller_b.def_id);
        span_diagnostic_ord(caller_a_span, caller_b_span)
    });
    callers
}

/// Records the calls of the caller at the distance, and adds its callees to the found callees.
fn record_caller_calls<'tcx>(
    tcx: TyCtxt<'tcx>,
    call_graph: &mut CallGraph<'tcx>,
    caller: Callee<'tcx>,
    distance: usize,
    found_callees: &mut FxHashSet<Callee<'tcx>>,
) {
    let body_mir = tcx.instance_mir(ty::InstanceKind::Item(caller.def_id));

    let mut calls = mir_callees(tcx, &body_mir, caller.generic_args).collect::<Vec<_>>();
    calls.extend(drop_glue_callees(tcx, &body_mir, caller.generic_args));
    // HACK: We must sort the calls into a stable order for the corresponding diagnostics to be printed in a stable order.
    calls.sort_unstable_by(|call_a, call_b| span_diagnostic_ord(call_a.span, call_b.span));

    for call in calls {
        let Some(callee) = resolve_callee(tcx, call_graph, caller, call) else { continue; };

        let caller_calls = call_graph.nested_calls[distance].entry(caller).or_default();
        caller_calls.push(InstanceCall { callee, safety: call.safety, span: call.span });

        found_callees.insert(callee);
    }
}

/// Records the calls of the callers at the distance, and returns the callees at the next distance and the number of
/// callers ignored at the depth limit. The callees of a caller that does not add to the distance join the callers.
fn record_calls_at_distance<'tcx>(
    tcx: TyCtxt<'tcx>,
    call_graph: &mut CallGraph<'tcx>,
    targeting: Targeting,
    already_recorded_callers: &mut FxHashSet<Callee<'tcx>>,
    mut callers: FxHashSet<Callee<'tcx>>,
    distance: usize,
    at_depth_limit: bool,
) -> (FxHashSet<Callee<'tcx>>, usize) {
    let mut next_distance_callees: FxHashSet<Callee<'tcx>> = Default::default();
    let mut ignored_callers: FxHashSet<Callee<'tcx>> = Default::default();

    while !callers.is_empty() {
        let mut same_distance_callees: FxHashSet<Callee<'tcx>> = Default::default();

        for caller in sort_callers_by_span(tcx, callers) {
            stop::abort_if_requested(tcx);

            // `const` functions, like other `const` scopes, cannot be mutated.
            if tcx.is_const_fn(caller.def_id) || already_recorded_callers.contains(&caller) || !tcx.is_mir_available(caller.def_id) { continue; }

            let counts_call_frame = targeting.counts_call_frame(caller.def_id);
            if at_depth_limit && counts_call_frame {
                ignored_callers.insert(caller);
                continue;
            }

            let found_callees = match counts_call_frame {
                true => &mut next_distance_callees,
                false => &mut same_distance_callees,
            };
            record_caller_calls(tcx, call_graph, caller, distance, found_callees);

            already_recorded_callers.insert(caller);
        }

        callers = same_distance_callees;
    }

    (next_distance_callees, ignored_callers.len())
}

pub fn reachable_fns<'ast, 'tcx, 'ent>(
    tcx: TyCtxt<'tcx>,
    def_res: &ast_lowering::DefResolutions,
    krate: &'ast ast::Crate,
    entry_points: EntryPoints<'ent>,
    targeting: Targeting,
    depth_limit: Option<usize>,
    trace_length_limit: Option<usize>,
) -> (CallGraph<'tcx>, Vec<Target>) {
    let mut call_graph = CallGraph {
        virtual_calls_count: 0,
        dynamic_calls_count: 0,
        foreign_calls_count: 0,
        root_calls: Default::default(),
        nested_calls: vec![],
    };

    let test_def_ids = match entry_points {
        EntryPoints::Tests(tests) => tests.iter().map(|test| test.def_id).collect::<FxHashSet<_>>(),
        EntryPoints::External => bug!("cannot generate call graph from external entry points"),
    };

    let mut previously_found_callees: FxHashSet<Callee<'tcx>> = Default::default();

    for entry_point in entry_points.iter() {
        let body_mir = tcx.instance_mir(ty::InstanceKind::Item(entry_point.body_local_def_id().to_def_id()));

        // NOTE: We expect entry points to be non-polymorphic (i.e. no type or const generic) functions.
        //       This is because we cannot build a complete call graph with uninstantiated type and const parameters.
        if body_mir.is_polymorphic {
            match entry_points {
                // Tests cannot be generic functions anyway, so this is a hard crash.
                EntryPoints::Tests(_) => tcx.dcx().fatal("encountered generic function test definition"),
                EntryPoints::External => unreachable!(),
            }
        }

        let mut calls = mir_callees(tcx, &body_mir, tcx.mk_args(&[])).collect::<Vec<_>>();
        calls.extend(drop_glue_callees(tcx, &body_mir, tcx.mk_args(&[])));
        // HACK: We must sort the calls into a stable order for the corresponding diagnostics to be printed in a stable order.
        calls.sort_unstable_by(|call_a, call_b| span_diagnostic_ord(call_a.span, call_b.span));

        let caller = Callee::new(entry_point.local_def_id.to_def_id(), tcx.mk_args(&[]));
        for call in calls {
            let Some(callee) = resolve_callee(tcx, &mut call_graph, caller, call) else { continue; };

            let test_calls = call_graph.root_calls.entry(entry_point.local_def_id).or_default();
            test_calls.push(InstanceCall { callee, safety: call.safety, span: call.span });

            previously_found_callees.insert(callee);
        }
    }

    let mut already_recorded_callers: FxHashSet<Callee<'tcx>> = Default::default();

    for distance in 0.. {
        // No remaining callers were found, exit early.
        if previously_found_callees.is_empty() { break; }

        // At the explicit call graph depth limit, only the calls of callers that do not add to the distance are recorded.
        let at_depth_limit = depth_limit.is_some_and(|depth_limit| distance + 1 >= depth_limit);

        call_graph.nested_calls.push(Default::default());
        let callers = mem::take(&mut previously_found_callees);
        let (next_distance_callees, ignored_callers_count) = record_calls_at_distance(tcx, &mut call_graph, targeting, &mut already_recorded_callers, callers, distance, at_depth_limit);
        previously_found_callees = next_distance_callees;

        // Remove the empty entry that was prepared for the nested calls at the last distance.
        if call_graph.nested_calls.last().is_some_and(|calls| calls.is_empty()) { call_graph.nested_calls.pop(); }

        if !at_depth_limit { continue; }

        // Warn about non-recorded callers because of explicit call graph depth limit.
        if let Some(depth_limit) = depth_limit && ignored_callers_count > 0 {
            let mut diagnostic = tcx.dcx().struct_warn("incomplete call graph due to explicit depth limit");
            diagnostic.note(format!("call graph depth limit is set to {depth_limit}"));
            diagnostic.note(match ignored_callers_count {
                1 => "ignoring 1 caller and its callees".to_owned(),
                _ => format!("ignoring {ignored_callers_count} callers and their callees"),
            });
            diagnostic.emit();
        }

        break;
    }

    // During the call tree walk along the call traces, for each target, we record
    // each entry point's most severe unsafety source of any of its call paths.
    // Safe items called from an unsafe context (dependencies) will be
    // marked `Unsafety::Tainted` with their corresponding unsafety source.
    //
    // ```ignore
    // [Safe] fn x { [None -> Safe]
    //     [Safe] fn y { [Some(EnclosingUnsafe) -> Unsafe(EnclosingUnsafe)]
    //         unsafe { [Some(Unsafe) -> Unsafe(Unsafe)]
    //             [Safe] fn z { [Some(Unsafe) -> Tainted(Unsafe)] }
    //         }
    //         [Safe] fn w { [Some(EnclosingUnsafe) -> Tainted(EnclosingUnsafe)] }
    //         [Unsafe(Unsafe)] unsafe fn u { [Some(Unsafe) -> Unsafe(Unsafe)]
    //             [Safe] fn v { [Some(Unsafe) -> Tainted(Unsafe)] }
    //             [Safe] fn w { [Some(Unsafe) -> Tainted(Unsafe)] }
    //         }
    //     }
    // }
    // ```

    struct CalleeLookupCache<'tcx, 'a> {
        call_graph: &'a CallGraph<'tcx>,
        cache: UnsafeCell<FxHashMap<Callee<'tcx>, Option<&'a [InstanceCall<'tcx>]>>>,
    }

    impl<'tcx, 'a> CalleeLookupCache<'tcx, 'a> {
        fn new(call_graph: &'a CallGraph<'tcx>) -> Self {
            Self { call_graph, cache: UnsafeCell::new(Default::default()) }
        }

        fn callees_of_nested_caller(&self, caller: Callee<'tcx>) -> &[InstanceCall<'tcx>] {
            // SAFETY: The lookup cache is an append-only map; existing entries are never modified.
            let cache = unsafe { &mut *self.cache.get() };

            let callees = cache.entry(caller).or_insert_with(|| self.call_graph.callees_of_nested_caller(caller));
            callees.unwrap_or_default()
        }
    }

    /// Records the targets reachable from the entry point with a 0-1 breadth-first search over (callee, unsafety) states.
    /// Only calls from counted frames add to the distance. A reach at no shorter distance, without more unsafety, changes no target.
    fn record_nested_targets<'ast, 'tcx>(
        tcx: TyCtxt<'tcx>,
        def_res: &ast_lowering::DefResolutions,
        krate: &'ast ast::Crate,
        test_def_ids: &FxHashSet<hir::LocalDefId>,
        callee_lookup_cache: &CalleeLookupCache<'tcx, '_>,
        entry_point: LocalEntryPoint,
        targeting: Targeting,
        root_calls: &[InstanceCall<'tcx>],
        root_unsafety: Option<UnsafeSource>,
        targets: &mut FxHashMap<hir::DefId, Target>,
        trace_length_limit: Option<usize>,
    ) {
        let mut reached: FxHashMap<Callee<'tcx>, (usize, Option<UnsafeSource>)> = Default::default();
        let mut queue: VecDeque<(Callee<'tcx>, Option<UnsafeSource>, usize)> = Default::default();

        for call in root_calls {
            if reaches_closer_or_more_unsafely(&mut reached, call.callee, 0, root_unsafety) {
                queue.push_back((call.callee, root_unsafety, 0));
            }
        }

        while let Some((caller, unsafety, distance)) = queue.pop_front() {
            stop::abort_if_requested(tcx);

            // `const` functions, like other `const` scopes, cannot be mutated.
            if tcx.is_const_fn(caller.def_id) { continue; }

            record_target(tcx, def_res, krate, test_def_ids, entry_point, targeting, caller, unsafety, distance, targets);

            let counts_call_frame = targeting.counts_call_frame(caller.def_id);
            let callee_distance = distance + counts_call_frame as usize;
            if let Some(trace_length_limit) = trace_length_limit && callee_distance >= trace_length_limit {
                tcx.dcx().warn("exceeded explicit call graph trace length limit");
                continue;
            }

            for &call in callee_lookup_cache.callees_of_nested_caller(caller) {
                let unsafety = match call.safety {
                    hir::Safety::Safe => unsafety,
                    hir::Safety::Unsafe => Some(UnsafeSource::Unsafe),
                };

                if !reaches_closer_or_more_unsafely(&mut reached, call.callee, callee_distance, unsafety) { continue; }
                match counts_call_frame {
                    true => queue.push_back((call.callee, unsafety, callee_distance)),
                    false => queue.push_front((call.callee, unsafety, callee_distance)),
                }
            }
        }
    }

    let callee_lookup_cache = CalleeLookupCache::new(&call_graph);
    let mut targets: FxHashMap<hir::DefId, Target> = Default::default();
    for (&entry_point, calls) in &call_graph.root_calls {
        let Some(def_item) = ast_lowering::find_def_in_ast(tcx, def_res, entry_point, krate) else { continue };
        let unsafety = match check_item_unsafety(def_item) {
            Unsafety::Unsafe(unsafe_source) => Some(unsafe_source),
            _ => None,
        };
        // HACK: We can discard any def body overrides for entry points, as we have already collected all call information from them.
        let entry_point = LocalEntryPoint { local_def_id: entry_point, body_local_def_id: None };
        record_nested_targets(tcx, def_res, krate, &test_def_ids, &callee_lookup_cache, entry_point, targeting, calls, unsafety, &mut targets, trace_length_limit);
    }

    (call_graph, targets.into_values().collect())
}
