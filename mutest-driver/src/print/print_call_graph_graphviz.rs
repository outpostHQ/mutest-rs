use std::hash::{Hash, Hasher};
use std::iter;

use mutest_emit::analysis::call_graph::{CallGraph, Callee, EntryPoints, InstanceCall, Target};
use rustc_data_structures::fx::{FxHashMap, FxHashSet, FxHasher};
use rustc_data_structures::smallvec::{SmallVec, smallvec};
use rustc_middle::ty::TyCtxt;
use rustc_span::bug;
use rustc_span::def_id::DefId;

use crate::config;

fn call_graph_node_def_id(def_id: DefId) -> String {
    format!("def_{}_{}", def_id.krate.index(), def_id.index.index())
}

/// `GenericArgsRef` hashes its interned pointer, which differs between runs, so disambiguate
/// generic args by their printed form to keep this id stable for checked-in `.stdout` fixtures.
fn call_graph_node_id<'tcx>(tcx: TyCtxt<'tcx>, callee: &Callee<'tcx>) -> String {
    if callee.generic_args.is_empty() { return call_graph_node_def_id(callee.def_id); }
    let hash = { let mut hasher = FxHasher::default(); callee.display_str(tcx).hash(&mut hasher); hasher.finish() };
    format!("def_{}_{}_{:x}", callee.def_id.krate.index(), callee.def_id.index.index(), hash)
}

fn escape_graphviz_label(label: &str) -> String {
    rustc_graphviz::LabelText::LabelStr(std::borrow::Cow::Borrowed(label)).to_dot_string()
}

struct CallGraphGraphvizCtx<'a, 'tcx> { tcx: TyCtxt<'tcx>, targets: &'a [Target], entry_point_filters: &'a [String], non_local_call_view: config::CallGraphNonLocalCallView, filtered_nodes: &'a FxHashSet<Callee<'tcx>> }

type CollapsedCallPaths<'tcx> = FxHashMap<Callee<'tcx>, SmallVec<[SmallVec<[Callee<'tcx>; 3]>; 1]>>;

pub(super) fn print_call_graph_graphviz<'tcx, 'ent>(tcx: TyCtxt<'tcx>, entry_points: EntryPoints<'ent>, call_graph: &CallGraph<'tcx>, targets: &[Target], entry_point_filters: &[String], non_local_call_view: config::CallGraphNonLocalCallView, filtered_nodes: &FxHashSet<Callee<'tcx>>) {
    println!("strict digraph {{");
    println!("  layout=dot;");
    println!("  rankdir=LR;");
    println!("  concentrate=true;");
    println!("  ranksep=1;");
    println!("  forcelabels=true;");

    // Distance markers. Also used as "anchors" for the depth-based ranks.
    println!("  node [shape=none];");
    println!("  _ [label=\"distance:\"];");
    print!("  _");
    for distance in 0..=call_graph.nested_calls.len() { print!(" -> {distance}"); }
    println!(" [arrowhead=none];");

    println!("  node [shape=ellipse];");

    println!("  subgraph cluster_tests {{");
    println!("    label=\"{}\";", match entry_points {
        EntryPoints::Tests(_) => "tests (entry points)",
        EntryPoints::External => unreachable!(),
    });
    println!("    rank=same");
    println!("    style=filled;");
    println!("    color=lightgray;");
    println!("    node [style=filled, color=white];");

    for entry_point in entry_points.iter() {
        let entry_point_path_str = entry_point.path_str(tcx);

        if !super::matches_entry_point_filters(&entry_point_path_str, entry_point_filters) { continue; }

        // TODO: Use different styling for ignored test, or use strikethrough.
        println!("    {} [label=\"{}\"];", call_graph_node_def_id(entry_point.local_def_id.to_def_id()), entry_point_path_str);
    }

    println!("  }}");

    let ctx = CallGraphGraphvizCtx { tcx, targets, entry_point_filters, non_local_call_view, filtered_nodes };
    let mut defined_callees: FxHashSet<Callee> = Default::default();
    let mut collapsed_call_paths: CollapsedCallPaths<'tcx> = Default::default();

    print_call_graph_graphviz_root_calls(&ctx, call_graph, &mut defined_callees, &mut collapsed_call_paths);
    print_call_graph_graphviz_nested_calls(&ctx, call_graph, &mut defined_callees, &mut collapsed_call_paths);

    println!("}}");
}

fn print_call_graph_graphviz_callee_node<'tcx>(ctx: &CallGraphGraphvizCtx<'_, 'tcx>, defined_callees: &mut FxHashSet<Callee<'tcx>>, callee: Callee<'tcx>) {
    if !defined_callees.insert(callee) { return; }

    if !callee.def_id.is_local() {
        if let config::CallGraphNonLocalCallView::Expand = ctx.non_local_call_view {
            println!("    {} [label={}, shape=plaintext, fontcolor=lightgray];", call_graph_node_id(ctx.tcx, &callee), escape_graphviz_label(&callee.display_str(ctx.tcx)));
        }
        return;
    }

    match ctx.targets.iter().any(|target| target.def_id() == callee.def_id) {
        true => println!("    {} [label={}];", call_graph_node_id(ctx.tcx, &callee), escape_graphviz_label(&callee.display_str(ctx.tcx))),
        false => println!("    {} [label={}, shape=plaintext];", call_graph_node_id(ctx.tcx, &callee), escape_graphviz_label(&callee.display_str(ctx.tcx))),
    }
}

fn print_call_graph_graphviz_root_calls<'tcx>(ctx: &CallGraphGraphvizCtx<'_, 'tcx>, call_graph: &CallGraph<'tcx>, defined_callees: &mut FxHashSet<Callee<'tcx>>, collapsed_call_paths: &mut CollapsedCallPaths<'tcx>) {
    println!("  {{ rank=same; 0;");
    let unique_root_callees = call_graph.root_calls.iter()
        .filter(|(root_def_id, _)| ctx.entry_point_filters.is_empty() || ctx.filtered_nodes.contains(&Callee::new(root_def_id.to_def_id(), ctx.tcx.mk_args(&[]))))
        .flat_map(|(_, calls)| calls)
        .map(|call| call.callee)
        .collect::<FxHashSet<_>>();
    for callee in super::sorted_callees_for_print(ctx.tcx, unique_root_callees) {
        print_call_graph_graphviz_callee_node(ctx, defined_callees, callee);
    }
    println!("  }}");

    for (root_def_id, calls) in &call_graph.root_calls {
        for call in calls {
            if !ctx.entry_point_filters.is_empty() && !ctx.filtered_nodes.contains(&Callee::new(root_def_id.to_def_id(), ctx.tcx.mk_args(&[]))) { continue; }
            print_call_graph_graphviz_root_call_edge(ctx.tcx, ctx.non_local_call_view, collapsed_call_paths, root_def_id.to_def_id(), call);
        }
    }
}

fn print_call_graph_graphviz_root_call_edge<'tcx>(tcx: TyCtxt<'tcx>, non_local_call_view: config::CallGraphNonLocalCallView, collapsed_call_paths: &mut CollapsedCallPaths<'tcx>, root_def_id: DefId, call: &InstanceCall<'tcx>) {
    if !call.callee.def_id.is_local() {
        match non_local_call_view {
            config::CallGraphNonLocalCallView::Collapse => {
                collapsed_call_paths.entry(call.callee).or_default().push(smallvec![Callee::new(root_def_id, tcx.mk_args(&[])), call.callee]);
            }
            config::CallGraphNonLocalCallView::Expand => {
                println!("  {} -> {}:w [color=lightgray];", call_graph_node_def_id(root_def_id), call_graph_node_id(tcx, &call.callee));
            }
        }
        return;
    }

    println!("  {} -> {}:w;", call_graph_node_def_id(root_def_id), call_graph_node_id(tcx, &call.callee));
}

fn print_call_graph_graphviz_nested_calls<'tcx>(ctx: &CallGraphGraphvizCtx<'_, 'tcx>, call_graph: &CallGraph<'tcx>, defined_callees: &mut FxHashSet<Callee<'tcx>>, collapsed_call_paths: &mut CollapsedCallPaths<'tcx>) {
    for (distance, calls) in iter::zip(1.., &call_graph.nested_calls) {
        println!("  {{ rank=same; {};", distance);
        let unique_nested_callees = calls.iter()
            .filter(|(caller, _)| ctx.entry_point_filters.is_empty() || ctx.filtered_nodes.contains(caller))
            .flat_map(|(_, calls)| calls)
            .map(|call| call.callee)
            .collect::<FxHashSet<_>>();
        for callee in super::sorted_callees_for_print(ctx.tcx, unique_nested_callees) {
            print_call_graph_graphviz_callee_node(ctx, defined_callees, callee);
        }
        println!("  }}");

        print_call_graph_graphviz_nested_call_edges(ctx, collapsed_call_paths, calls);
    }
}

/// Non-local callers in this bucket may themselves be callees of each other (non-local frames
/// do not increase distance), so edges are resolved in dependency order, not map iteration order.
fn print_call_graph_graphviz_nested_call_edges<'tcx>(ctx: &CallGraphGraphvizCtx<'_, 'tcx>, collapsed_call_paths: &mut CollapsedCallPaths<'tcx>, calls: &FxHashMap<Callee<'tcx>, Vec<InstanceCall<'tcx>>>) {
    let mut pending: Vec<(Callee<'tcx>, &InstanceCall<'tcx>)> = calls.iter()
        .filter(|(caller, _)| ctx.entry_point_filters.is_empty() || ctx.filtered_nodes.contains(caller))
        .flat_map(|(caller, calls)| calls.iter().map(move |call| (*caller, call)))
        .collect();
    // Deterministic order, for reproducible output: `partition` below preserves it.
    pending.sort_by(|(caller_a, call_a), (caller_b, call_b)| {
        super::call_graph_graphviz_print_order(ctx.tcx, caller_a, caller_b).then_with(|| super::call_graph_graphviz_print_order(ctx.tcx, &call_a.callee, &call_b.callee))
    });

    let caller_is_ready = |collapsed_call_paths: &CollapsedCallPaths<'tcx>, caller: &Callee<'tcx>| {
        caller.def_id.is_local() || matches!(ctx.non_local_call_view, config::CallGraphNonLocalCallView::Expand) || collapsed_call_paths.contains_key(caller)
    };

    while !pending.is_empty() {
        let (ready, not_ready) = pending.into_iter()
            .partition::<Vec<_>, _>(|(caller, _)| caller_is_ready(collapsed_call_paths, caller));
        if ready.is_empty() { bug!("cyclic or unregistered non-local callers while printing call graph"); }
        for (caller, call) in ready {
            print_call_graph_graphviz_nested_call_edge(ctx.tcx, ctx.non_local_call_view, collapsed_call_paths, caller, call);
        }
        pending = not_ready;
    }
}

fn print_call_graph_graphviz_nested_call_edge<'tcx>(tcx: TyCtxt<'tcx>, non_local_call_view: config::CallGraphNonLocalCallView, collapsed_call_paths: &mut CollapsedCallPaths<'tcx>, caller: Callee<'tcx>, call: &InstanceCall<'tcx>) {
    if caller.def_id.is_local() && call.callee.def_id.is_local() {
        println!("  {} -> {}:w;", call_graph_node_id(tcx, &caller), call_graph_node_id(tcx, &call.callee));
        return;
    }

    match non_local_call_view {
        config::CallGraphNonLocalCallView::Collapse => {
            print_call_graph_graphviz_collapsed_nested_call_edge(tcx, collapsed_call_paths, caller, call);
        }
        config::CallGraphNonLocalCallView::Expand => {
            println!("  {} -> {}:w [color=lightgray];", call_graph_node_id(tcx, &caller), call_graph_node_id(tcx, &call.callee));
        }
    }
}

fn print_call_graph_graphviz_collapsed_nested_call_edge<'tcx>(tcx: TyCtxt<'tcx>, collapsed_call_paths: &mut CollapsedCallPaths<'tcx>, caller: Callee<'tcx>, call: &InstanceCall<'tcx>) {
    match (caller.def_id.is_local(), call.callee.def_id.is_local()) {
        (true, true) => unreachable!(),
        // Local definition calls into non-local definition.
        (true, false) => {
            // Record new collapsed non-local call path.
            collapsed_call_paths.entry(call.callee).or_default().push(smallvec![caller, call.callee]);
        }
        // Non-local definition calls into other non-local definition.
        (false, false) => {
            // Extend collapsed non-local call paths.
            let Some(call_paths) = collapsed_call_paths.get(&caller) else { unreachable!() };
            let mut call_paths = call_paths.clone();
            for call_path in &mut call_paths {
                call_path.push(call.callee);
            }
            collapsed_call_paths.entry(call.callee).or_default().extend(call_paths);
        }
        // Non-local definition calls into local definition.
        (false, true) => {
            // Write out collapsed non-local call paths.
            let Some(call_paths) = collapsed_call_paths.get(&caller) else { unreachable!(); };
            print_call_graph_graphviz_write_collapsed_call_paths(tcx, call_paths, call.callee);
        }
    }
}

fn print_call_graph_graphviz_write_collapsed_call_paths<'tcx>(tcx: TyCtxt<'tcx>, call_paths: &SmallVec<[SmallVec<[Callee<'tcx>; 3]>; 1]>, callee: Callee<'tcx>) {
    for call_path in call_paths {
        let [root_callee, ..] = &call_path[..] else { unreachable!() };
        let tooltip = call_path.iter().chain(iter::once(&callee))
            .map(|callee| callee.display_str(tcx))
            .intersperse("\n-> ".to_owned())
            .collect::<String>();
        println!("  {} -> {}:w [label=\"indirect\", color=lightgray, labeltooltip={}];", call_graph_node_id(tcx, root_callee), call_graph_node_id(tcx, &callee), escape_graphviz_label(&tooltip));
    }
}
