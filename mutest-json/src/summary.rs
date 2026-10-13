//! The summary of a `cargo mutest` run, which it writes to `summary.json` in the JSON output root directory:
//! what the run prints for a reader as text, such as the totals of each evaluation.

use serde::{Serialize, Deserialize};

use crate::evaluation::MutationDetectionStats;

/// The environment variable that names the file each test harness appends its [`HarnessSummary`] to, as one line.
pub const LOG_VAR: &str = "MUTEST_SUMMARY_LOG";

/// The summary of a `cargo mutest` run.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct RunSummary {
    /// See [`crate::FORMAT_VERSION`].
    pub format_version: u32,
    /// The code `cargo mutest` exits with.
    pub exit_code: i32,
    /// Each test harness that did not build, so none of its mutations were evaluated
    /// (e.g. "the test harness of `krate` in package `krate`").
    pub failed_builds: Vec<String>,
    /// The summary of each evaluation, ordered by test harness. A `--flakes=N` run evaluates each test harness N times.
    pub harnesses: Vec<HarnessSummary>,
}

/// The kind of Cargo target a test harness is built from.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    Lib,
    MainBin,
    Bin,
    Example,
    Test,
}

/// The summary of one evaluation of a test harness.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct HarnessSummary {
    /// The Cargo package of the test harness, or [`None`] if it was built without Cargo.
    pub package: Option<String>,
    /// The kind of Cargo target of the test harness, or [`None`] if it was built without Cargo.
    pub target_kind: Option<TargetKind>,
    /// The crate name of the target, which is the integration test itself for [`TargetKind::Test`].
    pub target: String,
    /// The directory of the JSON metadata of the test harness, relative to the JSON output root directory,
    /// with `/` between components, and empty if the test harness was built without Cargo.
    pub metadata_dir: String,
    pub outcome: HarnessOutcome,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum HarnessOutcome {
    /// The mutations were evaluated.
    Evaluated {
        all_mutations_detection_stats: MutationDetectionStats,
        safe_mutations_detection_stats: MutationDetectionStats,
        unsafe_mutations_detection_stats: MutationDetectionStats,
        /// The mutations that timed out within short limits, which ran again alone with longer limits.
        timeout_reruns: TimeoutReruns,
    },
    /// Tests failed before any mutation, so no mutation was evaluated.
    ReferenceFailed {
        /// The names of the tests that failed.
        failed_tests: Vec<String>,
    },
    /// An integration test that links no mutant crate, so no mutation can reach its tests, and they were skipped.
    Skipped,
}

/// The results of the timed-out mutations that ran again alone.
#[derive(Clone, Eq, PartialEq, Debug, Serialize, Deserialize)]
pub struct TimeoutReruns {
    pub detected_count: usize,
    pub undetected_count: usize,
    pub crashed_count: usize,
    pub timed_out_count: usize,
}
