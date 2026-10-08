//! A loaded machine can make a test pass a short limit, so each mutation that timed out within short limits runs again
//! after the analysis, one test at a time, with the tests that did not pass it; isolated mutations run side by side.

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

/// A limit under this is short: load alone can use it up, and a rerun of its test costs little.
/// A test that passes a longer limit took five times its reference time, which load does not explain.
const SHORT_LIMIT: Duration = Duration::from_secs(20);

/// The limit of a test when its timed-out mutation runs again: a short limit gets ten seconds more.
pub(super) fn confirmation_timeout(timeout: Duration) -> Duration {
    if timeout < SHORT_LIMIT { timeout + Duration::from_secs(10) } else { timeout }
}

/// Whether no timeout is sure yet: each limit that a test passed is short.
fn all_short(limits: impl IntoIterator<Item = Option<Duration>>) -> bool {
    limits.into_iter().all(|limit| limit.is_none_or(|limit| limit < SHORT_LIMIT))
}

/// The limits of the tests that timed out with the mutation.
fn passed_limits<'a>(tests: &'a [test_runner::Test], mutation_result: &'a MutationTestResults) -> impl Iterator<Item = Option<Duration>> + 'a {
    let timed_out = |test: &&test_runner::Test| matches!(mutation_result.results_per_test.get(&test.desc.name), Some(Some(MutationTestResult::TimedOut)));
    tests.iter().filter(timed_out).map(|test| test.timeout)
}

/// Defers a mutation that timed out within short limits to `confirm_timeouts`, and records any other result.
pub(super) fn finish_mutation(results: &mut MutationAnalysisResults, timed_out_mutations: &mut Vec<(Mutant, &'static MutationMeta, MutationTestResults)>, journal: Option<&WorkerJournal>, tests: &[test_runner::Test], mutant: Mutant, mutation: &'static MutationMeta, mutation_result: MutationTestResults) {
    match mutation_result.result {
        MutationTestResult::TimedOut if all_short(passed_limits(tests, &mutation_result)) => timed_out_mutations.push((mutant, mutation, mutation_result)),
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
    fn a_test_gets_five_times_its_reference_time_and_at_least_a_second_more() {
        let limit = |millis| super::super::profiling::auto_test_timeout(Duration::from_millis(millis));
        assert_eq!((limit(10), limit(250), limit(60_000)), (Duration::from_millis(1010), Duration::from_millis(1250), Duration::from_secs(300)));
    }

    #[test]
    fn only_a_timeout_within_short_limits_runs_again() {
        let secs = Duration::from_secs;
        assert!(all_short([Some(secs(1)), Some(secs(19))]));
        assert!(!all_short([Some(secs(1)), Some(secs(20))]));
        assert!(all_short([]));
        assert_eq!((confirmation_timeout(secs(1)), confirmation_timeout(secs(19))), (secs(11), secs(29)));
        assert_eq!(confirmation_timeout(secs(20)), secs(20));
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
