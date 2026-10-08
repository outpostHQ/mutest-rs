//! A loaded machine can make a test time out, so each timed-out mutation runs again after the analysis, one test at a
//! time, with longer limits, and only with the tests that did not pass it; isolated mutations run side by side.

use std::iter;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use super::{LingeringTestMonitoringThread, MutationAnalysis, MutationAnalysisResults, MutationTestResult, MutationTestResults, alone, clone_tests, evaluate_alone, record_finished_mutation, run_tests, test};
use crate::journal::{self, WorkerJournal};
use crate::test_runner;
use crate::metadata::{Mutant, MutationMeta, SubstMap};
use crate::thread_pool::ThreadPool;
use crate::write::EvaluationStreamWriter;

/// The results of the timed-out mutations that ran again alone, with longer limits.
#[derive(Clone, Copy, Default)]
pub struct TimeoutReruns {
    pub detected_count: usize,
    pub undetected_count: usize,
    pub crashed_count: usize,
    pub timed_out_count: usize,
}

impl TimeoutReruns {
    pub(crate) fn record(&mut self, result: MutationTestResult) {
        let count = match result {
            MutationTestResult::Detected => &mut self.detected_count,
            MutationTestResult::Undetected => &mut self.undetected_count,
            MutationTestResult::Crashed => &mut self.crashed_count,
            MutationTestResult::TimedOut => &mut self.timed_out_count,
        };
        *count += 1;
    }

    pub fn rerun_count(&self) -> usize {
        self.detected_count + self.undetected_count + self.crashed_count + self.timed_out_count
    }

    /// Prints the line that tells a reader of the output that every remaining timeout is confirmed.
    pub(super) fn print(&self) {
        println!("timeouts confirmed: {rerun} re-run alone; {detected} detected, {undetected} undetected, {crashed} crashed, {timed_out} timed out again",
            rerun = self.rerun_count(),
            detected = self.detected_count,
            undetected = self.undetected_count,
            crashed = self.crashed_count,
            timed_out = self.timed_out_count,
        );
    }
}

/// The limit of a test when its timed-out mutation runs again: five times its first limit, and at least ten seconds more.
pub(super) fn confirmation_timeout(timeout: Duration) -> Duration {
    Ord::max(timeout * 5, timeout + Duration::from_secs(10))
}

/// Defers a timed-out mutation to `confirm_timeouts`, and records any other result.
pub(super) fn finish_mutation(results: &mut MutationAnalysisResults, timed_out_mutations: &mut Vec<(Mutant, &'static MutationMeta, MutationTestResults)>, journal: Option<&WorkerJournal>, mutant: Mutant, mutation: &'static MutationMeta, mutation_result: MutationTestResults) {
    match mutation_result.result {
        MutationTestResult::TimedOut => timed_out_mutations.push((mutant, mutation, mutation_result)),
        _ => record_finished_mutation(results, journal, mutation, mutation_result, false),
    }
}

pub(super) fn confirm_timeouts<S: SubstMap + Sync>(
    analysis: &MutationAnalysis<'_, S>,
    timed_out_mutations: Vec<(Mutant, &'static MutationMeta, MutationTestResults)>,
    results: &mut MutationAnalysisResults,
    thread_pool: Option<ThreadPool>,
    lingering_test_monitoring_thread: &Arc<LingeringTestMonitoringThread>,
    eval_stream_writer: Option<EvaluationStreamWriter>,
    journal: Option<&WorkerJournal>,
) {
    let (isolated, in_process): (Vec<_>, Vec<_>) = timed_out_mutations.into_iter()
        .partition(|&(_, mutation, _)| journal::isolates(journal, analysis.opts.mutation_isolation, &[mutation]));
    for (mutant, mutation, analysis_results) in in_process {
        let tests = not_passed(analysis.tests, &analysis_results);
        let rerun_results = evaluate_alone(&MutationAnalysis { tests: &tests, ..*analysis }, (mutant, mutation), true, thread_pool.clone(), lingering_test_monitoring_thread, eval_stream_writer.clone(), journal);
        finish_rerun(results, journal, mutation, analysis_results, rerun_results);
    }

    let reruns = isolated.iter().map(|(mutant, mutation, analysis_results)| {
        let tests = not_passed(analysis.tests, analysis_results);
        (*mutant, *mutation, alone::tests(&MutationAnalysis { tests: &tests, ..*analysis }, mutation, true))
    }).collect::<Vec<_>>();
    let rerun_results = side_by_side(reruns, analysis, thread_pool, lingering_test_monitoring_thread, eval_stream_writer);
    for ((_, mutation, analysis_results), rerun_results) in iter::zip(isolated, rerun_results) {
        finish_rerun(results, journal, mutation, analysis_results, rerun_results);
    }
}

/// Runs the tests of isolated mutations, which share no process, side by side on at most half the cores, so each still
/// runs with little load; gives their results in the order of `reruns`.
fn side_by_side<S: SubstMap + Sync>(reruns: Vec<(Mutant, &'static MutationMeta, Vec<test_runner::Test>)>, analysis: &MutationAnalysis<'_, S>, thread_pool: Option<ThreadPool>, lingering_test_monitoring_thread: &LingeringTestMonitoringThread, eval_stream_writer: Option<EvaluationStreamWriter>) -> Vec<MutationTestResults> {
    let (exhaustive, verbosity, external_tests_extra) = (analysis.opts.exhaustive, analysis.opts.verbosity, analysis.external_tests_extra);
    let width = thread::available_parallelism().map_or(1, |cores| cores.get().div_ceil(2)).min(reruns.len());
    let queue = Mutex::new(reruns.into_iter().enumerate());
    let finished = Mutex::new(Vec::new());
    thread::scope(|scope| {
        for _ in 0..width {
            let (thread_pool, eval_stream_writer, queue, finished) = (thread_pool.clone(), eval_stream_writer.clone(), &queue, &finished);
            scope.spawn(move || loop {
                let Some((index, (mutant, mutation, tests))) = queue.lock().unwrap().next() else { break };
                let (mut run_results, lingering_tests) = run_tests(tests, external_tests_extra, mutant, exhaustive, true, thread_pool.clone(), Some(1), eval_stream_writer.clone(), verbosity);
                lingering_test_monitoring_thread.submit_lingering_tests(lingering_tests);
                let Some(rerun_results) = run_results.remove(&mutation.id) else { unreachable!() };
                finished.lock().unwrap().push((index, rerun_results));
            });
        }
    });
    let mut finished = finished.into_inner().unwrap();
    finished.sort_unstable_by_key(|&(index, _)| index);
    finished.into_iter().map(|(_, rerun_results)| rerun_results).collect()
}

/// The tests that did not pass the mutation in the analysis, which its rerun runs again.
fn not_passed(tests: &[test_runner::Test], analysis_results: &MutationTestResults) -> Vec<test_runner::Test> {
    clone_tests(tests.iter().filter(|test| !passed(analysis_results, &test.desc.name)))
}

fn finish_rerun(results: &mut MutationAnalysisResults, journal: Option<&WorkerJournal>, mutation: &'static MutationMeta, analysis_results: MutationTestResults, rerun_results: MutationTestResults) {
    results.timeout_reruns.record(rerun_results.result);
    record_finished_mutation(results, journal, mutation, merged(analysis_results, rerun_results), true);
}

fn passed(results: &MutationTestResults, test: &test::TestName) -> bool {
    matches!(results.results_per_test.get(test), Some(Some(MutationTestResult::Undetected)))
}

/// The tests that passed the mutation in the analysis keep their results; the tests that ran again take their new ones.
fn merged(analysis_results: MutationTestResults, rerun_results: MutationTestResults) -> MutationTestResults {
    let mut results_per_test = analysis_results.results_per_test;
    results_per_test.retain(|_, result| matches!(result, Some(MutationTestResult::Undetected)));
    results_per_test.extend(rerun_results.results_per_test);
    MutationTestResults { result: rerun_results.result, results_per_test }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn results(result: MutationTestResult, tests: &[(&'static str, MutationTestResult)]) -> MutationTestResults {
        let results_per_test = tests.iter().map(|&(name, result)| (test::StaticTestName(name), Some(result))).collect();
        MutationTestResults { result, results_per_test }
    }

    #[test]
    fn timeout_rerun_runs_only_the_tests_that_did_not_pass() {
        let analysis_results = results(MutationTestResult::TimedOut, &[("passed", MutationTestResult::Undetected), ("slow", MutationTestResult::TimedOut), ("crashed", MutationTestResult::Crashed)]);
        assert!(passed(&analysis_results, &test::StaticTestName("passed")));
        assert!(!passed(&analysis_results, &test::StaticTestName("slow")));
        assert!(!passed(&analysis_results, &test::StaticTestName("not run")));

        let merged = merged(analysis_results, results(MutationTestResult::Undetected, &[("slow", MutationTestResult::Undetected)]));
        assert_eq!(merged.result, MutationTestResult::Undetected);
        let expected = results(MutationTestResult::Undetected, &[("passed", MutationTestResult::Undetected), ("slow", MutationTestResult::Undetected)]);
        assert_eq!(merged.results_per_test, expected.results_per_test);
    }
}
