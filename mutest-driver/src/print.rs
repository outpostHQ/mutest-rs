use std::iter;

use mutest_emit::analysis::call_graph::{CallGraph, Callee, EntryPoints, LocalEntryPoint, Target, TargetReachability, Unsafety};
use mutest_emit::analysis::tests::Test;
use mutest_emit::codegen::symbols::span_diagnostic_ord;
use mutest_emit::codegen::mutation::{Mut, MutId, MutationBatch, MutationConflictGraph, UnsafeTargeting};
use rustc_data_structures::fx::{FxHashMap, FxHashSet};
use rustc_middle::ty::TyCtxt;
use rustc_span::bug;
use rustc_span::def_id::DefId;

use crate::config;

mod print_call_graph_graphviz;

pub fn print_tests(tests: &[Test]) {
    let mut ignored_tests_count = 0;

    let mut tests_in_print_order = tests.iter()
        .map(|test| (test.path_str(), test))
        .collect::<Vec<_>>();
    tests_in_print_order.sort_unstable_by(|(test_a_path_str, _), (test_b_path_str, _)| Ord::cmp(test_a_path_str, test_b_path_str));

    let tests_count = tests_in_print_order.len();

    for (test_path_str, test) in tests_in_print_order {
        let mut marker = "";
        if test.ignore {
            marker = " [ignored]";
            ignored_tests_count += 1;
        }

        println!("test {test}{marker}",
            test = test_path_str,
        );
    }
    println!();

    println!("tests: {total} total; {ignored} ignored",
        total = tests_count,
        ignored = ignored_tests_count,
    );
}

pub fn print_targets<'tcx, 'trg>(tcx: TyCtxt<'tcx>, crate_kind: &config::CrateKind, targets: &'trg [Target], unsafe_targeting: UnsafeTargeting) {
    if let config::CrateKind::MutantForExternalTests(_) = crate_kind {
        bug!("cannot print mutation targets for external entry points");
    }

    let mut unsafe_targets_count = 0;
    let mut tainted_targets_count = 0;

    // Targets are printed in source span order.
    let mut targets_in_print_order = targets.iter()
        .map(|target| (tcx.def_span(target.def_id()), target))
        .collect::<Vec<_>>();
    targets_in_print_order.sort_unstable_by(|(target_a_span, _), (target_b_span, _)| span_diagnostic_ord(*target_a_span, *target_b_span));

    let targets_count = targets_in_print_order.len();

    for (target_span, target) in targets_in_print_order {
        let mut unsafe_marker = "";
        match (target.unsafety.is_unsafe(unsafe_targeting), target.unsafety) {
            (true, Unsafety::Tainted(_)) => {
                unsafe_targets_count += 1;
                tainted_targets_count += 1;
                unsafe_marker = "[tainted] ";
            }
            (true, _) => {
                unsafe_targets_count += 1;
                unsafe_marker = "[unsafe] ";
            }
            (false, _) => {}
        };

        println!("{entry_points}{reachability} {unsafe_marker}{def_path} at {span:#?}",
            entry_points = match crate_kind {
                config::CrateKind::MutantWithInternalTests => "tests",
                config::CrateKind::MutantForExternalTests(_) => unreachable!(),
                config::CrateKind::IntegrationTest => "tests",
            },
            reachability = match target.reachability {
                TargetReachability::DirectEntry => "".to_owned(),
                TargetReachability::NestedCallee { distance } => format!(" -({distance})->"),
            },
            def_path = tcx.def_path_str(target.def_id()),
            span = target_span,
        );

        // Entry points are printed in order of distance first, within that by lexical order of their definition path.
        let mut entry_points_in_print_order = target.reachable_from.iter()
            .map(|(entry_point, entry_point_assoc)| (entry_point.path_str(tcx), entry_point, entry_point_assoc))
            .collect::<Vec<_>>();
        entry_points_in_print_order.sort_unstable_by(|(entry_point_a_path_str, _, entry_point_a), (entry_point_b_path_str, _, entry_point_b)| {
            Ord::cmp(&entry_point_a.distance, &entry_point_b.distance).then(Ord::cmp(entry_point_a_path_str, entry_point_b_path_str))
        });

        for (entry_point_path_str, entry_point, entry_point_assoc) in entry_points_in_print_order {
            println!("  ({distance}) {tainted_marker}{entry_point}",
                distance = entry_point_assoc.distance,
                tainted_marker = match target.is_tainted(entry_point, unsafe_targeting) {
                    true => "[tainted] ",
                    false => "",
                },
                entry_point = entry_point_path_str,
            );
        }

        println!();
    }

    println!("targets: {total} total; {safe} safe; {unsafe} unsafe ({tainted} tainted)",
        total = targets_count,
        safe = targets_count - unsafe_targets_count,
        r#unsafe = unsafe_targets_count,
        tainted = tainted_targets_count,
    );
}

pub fn print_unreached_fns(tcx: TyCtxt<'_>, unreached_fns: &[(DefId, Option<usize>)]) {
    // Functions are printed in source span order.
    let mut fns_in_print_order = unreached_fns.iter()
        .map(|&(def_id, distance)| (tcx.def_span(def_id), def_id, distance))
        .collect::<Vec<_>>();
    fns_in_print_order.sort_unstable_by(|(span_a, ..), (span_b, ..)| span_diagnostic_ord(*span_a, *span_b));

    let mut too_distant_fns_count = 0;

    for (span, def_id, distance) in fns_in_print_order {
        let reachability = match distance {
            Some(distance) => {
                too_distant_fns_count += 1;
                format!("tests -({distance})->")
            }
            None => "no tests".to_owned(),
        };
        println!("{reachability} {def_path} at {span:#?}", def_path = tcx.def_path_str(def_id));
    }

    println!("unreached: {total} total; {unreached} not reached; {too_distant} beyond mutation depth",
        total = unreached_fns.len(),
        unreached = unreached_fns.len() - too_distant_fns_count,
        too_distant = too_distant_fns_count,
    );
}

fn matches_entry_point_filters(entry_point_path_str: &str, entry_point_filters: &[String]) -> bool {
    entry_point_filters.is_empty() || entry_point_filters.iter().any(|entry_point_filter| entry_point_path_str.contains(entry_point_filter))
}

/// Returns the entry points that match the filters, and all the callees they reach, through calls at any distance.
fn filtered_call_graph_nodes<'tcx>(tcx: TyCtxt<'tcx>, entry_points: EntryPoints<'_>, call_graph: &CallGraph<'tcx>, entry_point_filters: &[String]) -> FxHashSet<Callee<'tcx>> {
    let mut filtered_nodes: FxHashSet<Callee<'tcx>> = Default::default();
    if entry_point_filters.is_empty() { return filtered_nodes; }

    let mut callees_to_visit = vec![];
    for entry_point in entry_points.iter() {
        if !matches_entry_point_filters(&entry_point.path_str(tcx), entry_point_filters) { continue; }
        filtered_nodes.insert(Callee::new(entry_point.local_def_id.to_def_id(), tcx.mk_args(&[])));
        callees_to_visit.extend(call_graph.root_calls.get(&entry_point.local_def_id).map(|v| &**v).unwrap_or_default().iter().map(|call| call.callee));
    }

    while let Some(callee) = callees_to_visit.pop() {
        if !filtered_nodes.insert(callee) { continue; }
        callees_to_visit.extend(call_graph.callees_of_nested_caller(callee).unwrap_or_default().iter().map(|call| call.callee));
    }

    filtered_nodes
}

pub fn print_call_graph<'tcx, 'ent>(tcx: TyCtxt<'tcx>, entry_points: EntryPoints<'ent>, call_graph: &CallGraph<'tcx>, targets: &[Target], format: config::GraphFormat, entry_point_filters: &[String], non_local_call_view: config::CallGraphNonLocalCallView) {
    if let EntryPoints::External = entry_points {
        bug!("cannot print call graph for external entry points");
    }

    let filtered_nodes = filtered_call_graph_nodes(tcx, entry_points, call_graph, entry_point_filters);

    match format {
        config::GraphFormat::Simple => print_call_graph_simple(tcx, entry_points, call_graph, entry_point_filters, &filtered_nodes),
        config::GraphFormat::Graphviz => print_call_graph_graphviz::print_call_graph_graphviz(tcx, entry_points, call_graph, targets, entry_point_filters, non_local_call_view, &filtered_nodes),
    }
}

fn print_call_graph_simple<'tcx, 'ent>(tcx: TyCtxt<'tcx>, entry_points: EntryPoints<'ent>, call_graph: &CallGraph<'tcx>, entry_point_filters: &[String], filtered_nodes: &FxHashSet<Callee<'tcx>>) {
    let mut entry_points_in_print_order = entry_points.iter()
        .map(|entry_point| (entry_point.path_str(tcx), entry_point))
        .collect::<Vec<_>>();
    entry_points_in_print_order.sort_unstable_by(|(entry_point_a_path_str, _), (entry_point_b_path_str, _)| Ord::cmp(entry_point_a_path_str, entry_point_b_path_str));

    println!("entry points:\n");

    for (entry_point_path_str, entry_point) in entry_points_in_print_order {
        if !matches_entry_point_filters(&entry_point_path_str, entry_point_filters) { continue; }

        println!("{descr} {path}",
            descr = match entry_points {
                EntryPoints::Tests(_) => "test",
                EntryPoints::External => unreachable!(),
            },
            path = entry_point_path_str,
        );

        let callees = call_graph.root_calls.get(&entry_point.local_def_id).map(|v| &**v).unwrap_or_default().iter()
            .map(|call| call.callee).collect::<FxHashSet<_>>();
        for callee in sorted_callees_for_print(tcx, callees) {
            println!("  -> {} at {:#?}", callee.display_str(tcx), tcx.def_span(callee.def_id));
        }
    }

    for (distance, calls) in iter::zip(1.., &call_graph.nested_calls) {
        println!("\nnested calls at distance {distance}:\n");

        let callers = calls.keys().copied()
            .filter(|caller| entry_point_filters.is_empty() || filtered_nodes.contains(caller))
            .collect::<FxHashSet<_>>();
        for caller in sorted_callees_for_print(tcx, callers) {
            println!("{} at {:#?}", caller.display_str(tcx), tcx.def_span(caller.def_id));

            let callees = calls.get(&caller).map(|v| &**v).unwrap_or_default().iter()
                .map(|call| call.callee).collect::<FxHashSet<_>>();
            for callee in sorted_callees_for_print(tcx, callees) {
                println!("  -> {} at {:#?}", callee.display_str(tcx), tcx.def_span(callee.def_id));
            }
        }
    }
}

fn call_graph_graphviz_print_order<'tcx>(tcx: TyCtxt<'tcx>, a: &Callee<'tcx>, b: &Callee<'tcx>) -> std::cmp::Ordering {
    span_diagnostic_ord(tcx.def_span(a.def_id), tcx.def_span(b.def_id)).then_with(|| Ord::cmp(&a.display_str(tcx), &b.display_str(tcx)))
}

/// `Callee` hashes its (per-run, interning-pointer-based) generic args, so a `FxHashSet<Callee>`
/// iterates in an order that varies between runs: sort it into a reproducible order before print.
fn sorted_callees_for_print<'tcx>(tcx: TyCtxt<'tcx>, callees: FxHashSet<Callee<'tcx>>) -> Vec<Callee<'tcx>> {
    let mut callees = callees.into_iter().collect::<Vec<_>>();
    callees.sort_unstable_by(|a, b| call_graph_graphviz_print_order(tcx, a, b));
    callees
}

pub fn print_mutation_graph<'trg: 'op, 'm: 'op, 'op, N, E>(mutation_conflict_graph: &MutationConflictGraph<'m>, mutations_iter: N, edge_iter: E, format: config::GraphFormat)
where
    N: IntoIterator<Item = &'op Mut<'trg, 'm>>,
    E: IntoIterator<Item = (MutId, MutId)>,
{
    match format {
        config::GraphFormat::Simple => {
            for (a, b) in edge_iter {
                println!("{} {}", a.index(), b.index());
            }
        }
        config::GraphFormat::Graphviz => {
            println!("strict graph {{");
            println!("  overlap=\"false\";");
            println!("  splines=\"true\";");

            for m in mutations_iter.into_iter() {
                let mut attrs = vec![];
                if mutation_conflict_graph.is_unsafe(m.id) {
                    attrs.push("color=\"red\"");
                    attrs.push("fontcolor=\"red\"");
                }

                println!("  {}{attrs};", m.id.index(),
                    attrs = match attrs.is_empty() {
                        true => "".to_owned(),
                        false => format!(" [{}]", attrs.join(", ")),
                    },
                );
            }

            for (a, b) in edge_iter.into_iter() {
                println!("  {} -- {};", a.index(), b.index());
            }

            println!("}}");
        }
    }
}

pub fn print_mutations<'tcx>(tcx: TyCtxt<'tcx>, mutations: &[Mut], mutation_batches: Option<&[MutationBatch]>, unsafe_targeting: UnsafeTargeting, verbosity: u8) {
    let mut total_mutations_count = 0;
    let mut unsafe_mutations_count = 0;
    let mut tainted_mutations_count = 0;
    let mut unbatched_mutations_count = 0;

    let mut print_mutation = |mutation: &Mut, nested: bool| {
        let mut unsafe_marker = "";
        match (mutation.is_unsafe(unsafe_targeting), mutation.target.unsafety) {
            (true, Unsafety::Tainted(_)) => {
                unsafe_mutations_count += 1;
                tainted_mutations_count += 1;
                unsafe_marker = "(tainted) ";
            }
            (true, _) => {
                unsafe_mutations_count += 1;
                unsafe_marker = "(unsafe) ";
            }
            (false, _) => {}
        };

        if nested { print!("  - "); }
        if verbosity >= 1 {
            print!("{}: ", mutation.id.index());
        }
        println!("{unsafe_marker}[{op_name}] {display_name} in {def_path} at {display_location}",
            op_name = mutation.op_name(),
            display_name = mutation.display_name(),
            def_path = tcx.def_path_str(mutation.target.def_id()),
            display_location = mutation.display_location(tcx.sess),
        );

        if let TargetReachability::DirectEntry = mutation.target.reachability {
            // NOTE: The entry point's original body override, if any, can be discarded here, as it is no longer used.
            let entry_point = LocalEntryPoint { local_def_id: mutation.target.def_id().expect_local(), body_local_def_id: None };

            if nested { print!("  "); }
            println!("  <- {entry_point} (direct entry point)",
                entry_point = entry_point.path_str(tcx),
            );
        }

        // Entry points are printed in order of distance first, within that by lexical order of their definition path.
        let mut entry_points_in_print_order = mutation.target.reachable_from.iter()
            .map(|(entry_point, entry_point_assoc)| (entry_point.path_str(tcx), entry_point, entry_point_assoc))
            .collect::<Vec<_>>();
        entry_points_in_print_order.sort_unstable_by(|(entry_point_a_path_str, _, entry_point_a), (entry_point_b_path_str, _, entry_point_b)| {
            Ord::cmp(&entry_point_a.distance, &entry_point_b.distance).then(Ord::cmp(entry_point_a_path_str, entry_point_b_path_str))
        });

        for (entry_point_path_str, entry_point, entry_point_assoc) in entry_points_in_print_order {
            if nested { print!("  "); }
            println!("  <-({distance})- {tainted_marker}{entry_point}",
                distance = entry_point_assoc.distance,
                tainted_marker = match mutation.target.is_tainted(entry_point, unsafe_targeting) {
                    true => "(tainted) ",
                    false => "",
                },
                entry_point = entry_point_path_str,
            );
        }
    };

    match mutation_batches {
        None => {
            // Mutations are printed in assigned ID order.
            for mutation in mutations {
                total_mutations_count += 1;
                print_mutation(mutation, false);
                println!();
            }
        }
        Some(mutation_batches) => {
            for mutation_batch in mutation_batches {
                total_mutations_count += mutation_batch.mutations.len();
                if mutation_batch.mutations.len() == 1 {
                    unbatched_mutations_count += 1;
                }

                if verbosity >= 1 {
                    print!("{}: ", mutation_batch.id.index());
                }
                match mutation_batch.mutations.len() {
                    1 => println!("batch of 1 mutation"),
                    n => println!("batch of {n} mutations"),
                };

                // Mutations are printed in assigned ID order.
                let mut mutations_in_print_order = mutation_batch.mutations.iter().collect::<Vec<_>>();
                mutations_in_print_order.sort_unstable_by_key(|mutation| mutation.id.index());

                for mutation in mutations_in_print_order {
                    print_mutation(mutation, true);
                }
                println!();
            }
        }
    }

    if verbosity >= 1 {
        #[derive(Clone, Copy, Default)]
        struct MutationOpStats {
            total_mutations_count: usize,
            unsafe_mutations_count: usize,
            tainted_mutations_count: usize,
            unbatched_mutations_count: usize,
        }

        let mut mutation_op_stats: FxHashMap<&str, MutationOpStats> = Default::default();
        for mutation in mutations {
            let op_stats = mutation_op_stats.entry(mutation.op_name()).or_default();
            op_stats.total_mutations_count += 1;
            if mutation.is_unsafe(unsafe_targeting) { op_stats.unsafe_mutations_count += 1; }
            if let Unsafety::Tainted(_) = mutation.target.unsafety { op_stats.tainted_mutations_count +=1; }
        }
        if let Some(mutation_batches) = mutation_batches {
            for mutation_batch in mutation_batches {
                if mutation_batch.mutations.len() > 1 { continue; }
                for mutation in &mutation_batch.mutations {
                    let Some(op_stats) = mutation_op_stats.get_mut(mutation.op_name()) else { unreachable!() };
                    op_stats.unbatched_mutations_count += 1;
                }
            }
        }

        let op_name_w = mutest_operators::ALL.iter().map(|s| s.len()).max().unwrap_or(0);
        let mutations_w = mutation_op_stats.values().map(|s| s.total_mutations_count.checked_ilog10().unwrap_or(0) as usize + 1).max().unwrap_or(0);
        let safe_w = mutation_op_stats.values().map(|s| (s.total_mutations_count - s.unsafe_mutations_count).checked_ilog10().unwrap_or(0) as usize + 1).max().unwrap_or(0);
        let unsafe_w = mutation_op_stats.values().map(|s| s.unsafe_mutations_count.checked_ilog10().unwrap_or(0) as usize + 1).max().unwrap_or(0);
        let tainted_w = mutation_op_stats.values().map(|s| s.tainted_mutations_count.checked_ilog10().unwrap_or(0) as usize + 1).max().unwrap_or(0);
        let batched_w = mutation_op_stats.values().map(|s| (s.total_mutations_count - s.unbatched_mutations_count).checked_ilog10().unwrap_or(0) as usize + 1).max().unwrap_or(0);
        let unbatched_w = mutation_op_stats.values().map(|s| s.unbatched_mutations_count.checked_ilog10().unwrap_or(0) as usize + 1).max().unwrap_or(0);

        // TODO: Only list statistics for active mutation operators.
        for op_name in mutest_operators::ALL {
            let op_stats = mutation_op_stats.get(op_name).map(|s| *s).unwrap_or_default();

            print!("{op_name:>op_name_w$}: {mutations_pct:>6}. {mutations:>mutations_w$} mutations; {safe:>safe_w$} safe; {unsafe:>unsafe_w$} unsafe ({tainted:>tainted_w$} tainted)",
                mutations_pct = format!("{:.2}%", op_stats.total_mutations_count as f64 / total_mutations_count as f64 * 100_f64),
                mutations = op_stats.total_mutations_count,
                safe = op_stats.total_mutations_count - op_stats.unsafe_mutations_count,
                r#unsafe = op_stats.unsafe_mutations_count,
                tainted = op_stats.tainted_mutations_count,
            );
            if let Some(_mutation_batches) = mutation_batches {
                print!("; {batched:>batched_w$} batched; {unbatched:>unbatched_w$} unbatched",
                    batched = op_stats.total_mutations_count - op_stats.unbatched_mutations_count,
                    unbatched = op_stats.unbatched_mutations_count,
                );
            }
            println!();
        }

        println!();
    }

    println!("{mutations} mutations; {safe} safe; {unsafe} unsafe ({tainted} tainted)",
        mutations = total_mutations_count,
        safe = total_mutations_count - unsafe_mutations_count,
        r#unsafe = unsafe_mutations_count,
        tainted = tainted_mutations_count,
    );
    if let Some(mutation_batches) = mutation_batches {
        println!("{batches} batches; {batched} batched; {unbatched} unbatched",
            batches = mutation_batches.len(),
            batched = total_mutations_count - unbatched_mutations_count,
            unbatched = unbatched_mutations_count,
        );
    }
}
