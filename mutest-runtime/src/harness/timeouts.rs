//! A loaded machine can make a test time out, so each timed-out mutation runs again after the analysis:
//! alone, one test at a time, with longer limits.

use std::sync::Arc;
use std::time::Duration;

use super::{LingeringTestMonitoringThread, MutationAnalysis, MutationAnalysisResults, MutationTestResult, MutationTestResults, evaluate_alone, record_finished_mutation};
use crate::journal::WorkerJournal;
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
pub(super) fn finish_mutation(results: &mut MutationAnalysisResults, timed_out_mutations: &mut Vec<(Mutant, &'static MutationMeta)>, journal: Option<&WorkerJournal>, mutant: Mutant, mutation: &'static MutationMeta, mutation_result: MutationTestResults) {
    match mutation_result.result {
        MutationTestResult::TimedOut => timed_out_mutations.push((mutant, mutation)),
        _ => record_finished_mutation(results, journal, mutation, mutation_result, false),
    }
}

pub(super) fn confirm_timeouts<S: SubstMap + Sync>(
    analysis: &MutationAnalysis<'_, S>,
    timed_out_mutations: Vec<(Mutant, &'static MutationMeta)>,
    results: &mut MutationAnalysisResults,
    thread_pool: Option<ThreadPool>,
    lingering_test_monitoring_thread: &Arc<LingeringTestMonitoringThread>,
    eval_stream_writer: Option<EvaluationStreamWriter>,
    journal: Option<&WorkerJournal>,
) {
    for timed_out_mutation in timed_out_mutations {
        let mutation_result = evaluate_alone(analysis, timed_out_mutation, true, thread_pool.clone(), lingering_test_monitoring_thread, eval_stream_writer.clone(), journal);
        results.timeout_reruns.record(mutation_result.result);
        record_finished_mutation(results, journal, timed_out_mutation.1, mutation_result, true);
    }
}
