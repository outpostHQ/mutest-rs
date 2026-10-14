//! The summary each test harness appends to the log that `cargo mutest` gathers into `summary.json`.

use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use mutest_json::summary::{HarnessOutcome, HarnessSummary, LOG_VAR, TargetKind, TimeoutReruns};

use crate::harness::MutationAnalysisResults;
use crate::json;
use crate::metadata::CargoTargetKind;
use crate::write::detection_stats;

/// The test harness that this process runs, which each summary it records names.
static HARNESS: OnceLock<Harness> = OnceLock::new();

/// The Cargo target of a test harness.
pub(crate) struct Harness {
    package: Option<&'static str>,
    target_kind: Option<CargoTargetKind>,
    target: &'static str,
    links_mutant_crate: bool,
}

impl Harness {
    /// The harness of the crate `crate_name`, or of the integration test `test_crate_name` that links it as its mutant crate.
    pub(crate) fn new(package: Option<&'static str>, target_kind: Option<CargoTargetKind>, crate_name: &'static str, test_crate_name: Option<&'static str>) -> Self {
        // NOTE: An integration test that links no mutant crate is built as its own, empty meta-mutant.
        let links_mutant_crate = target_kind != Some(CargoTargetKind::Test) || test_crate_name.is_some();
        Self { package, target_kind, target: test_crate_name.unwrap_or(crate_name), links_mutant_crate }
    }

    /// The directory of its JSON metadata under the JSON output root directory `root`.
    pub(crate) fn metadata_dir_under(&self, root: &Path) -> PathBuf {
        let mut dir = root.to_owned();
        dir.extend(self.metadata_dir());
        dir
    }

    fn metadata_dir(&self) -> Vec<&'static str> {
        let Some(package) = self.package else { return vec![]; };
        match self.target_kind {
            None => vec![package],
            Some(CargoTargetKind::Lib) => vec![package, "lib"],
            Some(CargoTargetKind::MainBin) => vec![package, "bin"],
            Some(CargoTargetKind::Bin) => vec![package, "bins", self.target],
            Some(CargoTargetKind::Example) => vec![package, "examples", self.target],
            Some(CargoTargetKind::Test) => vec![package, "tests", self.target],
        }
    }

    fn evaluation(&self, results: &MutationAnalysisResults) -> HarnessOutcome {
        if !self.links_mutant_crate { return HarnessOutcome::Skipped; }
        HarnessOutcome::Evaluated {
            all_mutations_detection_stats: detection_stats(results.all_stats()),
            safe_mutations_detection_stats: detection_stats(results.safe_stats()),
            unsafe_mutations_detection_stats: detection_stats(results.unsafe_stats()),
            timeout_reruns: TimeoutReruns {
                detected_count: results.timeout_reruns.detected_count,
                undetected_count: results.timeout_reruns.undetected_count,
                crashed_count: results.timeout_reruns.crashed_count,
                timed_out_count: results.timeout_reruns.timed_out_count,
            },
        }
    }

    fn summary(&self, outcome: HarnessOutcome) -> HarnessSummary {
        HarnessSummary {
            package: self.package.map(str::to_owned),
            target_kind: self.target_kind.map(target_kind),
            target: self.target.to_owned(),
            metadata_dir: self.metadata_dir().join("/"),
            outcome,
        }
    }
}

fn target_kind(kind: CargoTargetKind) -> TargetKind {
    match kind {
        CargoTargetKind::Lib => TargetKind::Lib,
        CargoTargetKind::MainBin => TargetKind::MainBin,
        CargoTargetKind::Bin => TargetKind::Bin,
        CargoTargetKind::Example => TargetKind::Example,
        CargoTargetKind::Test => TargetKind::Test,
    }
}

/// Sets the test harness that this process runs.
pub(crate) fn start(harness: Harness) {
    let _ = HARNESS.set(harness);
}

pub(crate) fn record_evaluation(results: &MutationAnalysisResults) {
    record(|harness| harness.evaluation(results));
}

pub(crate) fn record_reference_failure(failed_tests: Vec<String>) {
    record(|_| HarnessOutcome::ReferenceFailed { failed_tests });
}

/// Appends the summary to the log that `cargo mutest` names, if it names one.
fn record(outcome: impl FnOnce(&Harness) -> HarnessOutcome) {
    record_in(env::var_os(LOG_VAR).as_deref().map(Path::new), HARNESS.get(), outcome);
}

fn record_in(log: Option<&Path>, harness: Option<&Harness>, outcome: impl FnOnce(&Harness) -> HarnessOutcome) {
    let (Some(log), Some(harness)) = (log, harness) else { return; };
    let line = json::to_vec(&harness.summary(outcome(harness)));
    // NOTE: A newline first ends a line that a crash cut short, which would hide this line.
    append(log, line.map(|line| [&b"\n"[..], &line, b"\n"].concat()));
}

/// Appends `bytes` to `log` in one write, which keeps them whole among the lines of the other test harnesses.
fn append(log: &Path, bytes: io::Result<Vec<u8>>) {
    if let Err(error) = bytes.and_then(|bytes| OpenOptions::new().create(true).append(true).open(log)?.write_all(&bytes)) {
        eprintln!("warning: cannot record the summary of this test harness: {error}");
    }
}

/// Points a worker at `staging`, emptied first, as its log.
pub(crate) fn pass_to(worker: &mut Command, staging: &Path) {
    let _ = fs::remove_file(staging);
    worker.env(LOG_VAR, staging);
}

/// Appends the whole lines of the last worker's `staging` log to the log that `cargo mutest` names, so that a test
/// harness whose worker restarted logs its summary once.
pub(crate) fn hand_over(staging: &Path) {
    let (Some(log), Ok(mut lines)) = (env::var_os(LOG_VAR), fs::read(staging)) else { return };
    lines.truncate(crate::journal::whole_rows(&lines));
    append(Path::new(&log), Ok(lines));
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::fs;
    use std::path::Path;
    use std::process;
    use std::time::Duration;

    use mutest_json::summary::{HarnessOutcome, HarnessSummary, TargetKind};

    use super::{Harness, record_in};
    use crate::detections::MutationDetectionMatrix;
    use crate::harness::MutationAnalysisResults;
    use crate::json;
    use crate::metadata::CargoTargetKind;

    fn metadata_dir(harness: Harness) -> String {
        harness.metadata_dir().join("/")
    }

    #[test]
    fn each_target_keeps_its_metadata_in_its_own_directory() {
        assert_eq!(metadata_dir(Harness::new(Some("pkg"), Some(CargoTargetKind::Lib), "pkg", None)), "pkg/lib");
        assert_eq!(metadata_dir(Harness::new(Some("pkg"), Some(CargoTargetKind::MainBin), "pkg", None)), "pkg/bin");
        assert_eq!(metadata_dir(Harness::new(Some("pkg"), Some(CargoTargetKind::Bin), "tool", None)), "pkg/bins/tool");
        assert_eq!(metadata_dir(Harness::new(Some("pkg"), Some(CargoTargetKind::Example), "demo", None)), "pkg/examples/demo");
        assert_eq!(metadata_dir(Harness::new(Some("pkg"), Some(CargoTargetKind::Test), "pkg", Some("cli"))), "pkg/tests/cli");
        assert_eq!(metadata_dir(Harness::new(Some("pkg"), None, "pkg", None)), "pkg");
        assert_eq!(metadata_dir(Harness::new(None, None, "krate", None)), "");
        assert_eq!(Harness::new(Some("pkg"), Some(CargoTargetKind::Bin), "tool", None).metadata_dir_under(Path::new("out")), Path::new("out/pkg/bins/tool"));
    }

    #[test]
    fn only_an_integration_test_with_no_mutant_crate_links_none() {
        assert!(!Harness::new(Some("pkg"), Some(CargoTargetKind::Test), "cli", None).links_mutant_crate);
        assert!(Harness::new(Some("pkg"), Some(CargoTargetKind::Test), "pkg", Some("cli")).links_mutant_crate);
        assert!(Harness::new(Some("pkg"), Some(CargoTargetKind::Lib), "pkg", None).links_mutant_crate);
    }

    #[test]
    fn each_summary_is_one_line_of_the_json_that_cargo_mutest_reads() {
        let log = env::current_exe().unwrap().with_file_name(format!("mutest-summary-{}", process::id()));
        let _ = fs::remove_file(&log);
        let harness = Harness::new(Some("pkg"), Some(CargoTargetKind::Test), "pkg", Some("cli"));
        let krate = Harness::new(None, None, "krate", None);
        let failed = || HarnessOutcome::ReferenceFailed { failed_tests: vec!["a::b".to_owned()] };

        record_in(Some(&log), Some(&harness), |_| failed());
        record_in(Some(&log), Some(&krate), |_| HarnessOutcome::Skipped);

        let written = fs::read_to_string(&log).unwrap();
        fs::remove_file(&log).unwrap();
        assert_eq!(written, concat!(
            "\n", r#"{"package":"pkg","target_kind":"test","target":"cli","metadata_dir":"pkg/tests/cli","outcome":{"result":"reference_failed","failed_tests":["a::b"]}}"#, "\n",
            "\n", r#"{"package":null,"target_kind":null,"target":"krate","metadata_dir":"","outcome":{"result":"skipped"}}"#, "\n",
        ));
        let read = written.lines().filter(|line| !line.is_empty()).map(|line| serde_json::from_str::<HarnessSummary>(line).unwrap()).collect::<Vec<_>>();
        assert_eq!(read, [harness.summary(failed()), krate.summary(HarnessOutcome::Skipped)]);
    }

    #[test]
    fn an_evaluation_is_summarized_with_its_totals_unless_no_mutant_crate_is_linked() {
        let mut results = MutationAnalysisResults {
            total_mutations_count: 10,
            total_safe_mutations_count: 6,
            undetected_mutations_count: 1,
            undetected_safe_mutations_count: 1,
            timed_out_mutations_count: 1,
            timed_out_safe_mutations_count: 1,
            crashed_mutations_count: 1,
            crashed_safe_mutations_count: 1,
            mutation_detection_matrix: MutationDetectionMatrix::new(0),
            mutation_op_stats: Default::default(),
            timeout_reruns: Default::default(),
            duration: Duration::ZERO,
        };
        results.timeout_reruns.timed_out_count = 1;
        let lib = Harness::new(Some("pkg"), Some(CargoTargetKind::Lib), "pkg", None);
        let summary = lib.summary(lib.evaluation(&results));

        assert_eq!(String::from_utf8(json::to_vec(&summary).unwrap()).unwrap(), concat!(
            r#"{"package":"pkg","target_kind":"lib","target":"pkg","metadata_dir":"pkg/lib","outcome":{"result":"evaluated","#,
            r#""all_mutations_detection_stats":{"mutation_score":0.875,"total_mutations_count":10,"detected_mutations_count":7,"timed_out_mutations_count":1,"crashed_mutations_count":1,"undetected_mutations_count":1},"#,
            r#""safe_mutations_detection_stats":{"mutation_score":0.75,"total_mutations_count":6,"detected_mutations_count":3,"timed_out_mutations_count":1,"crashed_mutations_count":1,"undetected_mutations_count":1},"#,
            r#""unsafe_mutations_detection_stats":{"mutation_score":1.0,"total_mutations_count":4,"detected_mutations_count":4,"timed_out_mutations_count":0,"crashed_mutations_count":0,"undetected_mutations_count":0},"#,
            r#""timeout_reruns":{"detected_count":0,"undetected_count":0,"crashed_count":0,"timed_out_count":1}}}"#,
        ));
        let unlinked = Harness::new(Some("pkg"), Some(CargoTargetKind::Test), "cli", None);
        assert_eq!(unlinked.evaluation(&results), HarnessOutcome::Skipped);
    }

    #[test]
    fn each_cargo_target_kind_is_named_in_the_summary() {
        let kinds = [CargoTargetKind::Lib, CargoTargetKind::MainBin, CargoTargetKind::Bin, CargoTargetKind::Example, CargoTargetKind::Test]
            .map(|kind| Harness::new(Some("pkg"), Some(kind), "pkg", None).summary(HarnessOutcome::Skipped).target_kind);

        assert_eq!(kinds, [TargetKind::Lib, TargetKind::MainBin, TargetKind::Bin, TargetKind::Example, TargetKind::Test].map(Some));
    }

    #[test]
    fn a_harness_records_its_summary_only_once_started_and_given_a_log() {
        let log = env::current_exe().unwrap().with_file_name(format!("mutest-harness-summary-{}", process::id()));
        let _ = fs::remove_file(&log);
        let harness = Harness::new(Some("pkg"), Some(CargoTargetKind::Lib), "pkg", None);

        record_in(None, Some(&harness), |_| HarnessOutcome::Skipped);
        record_in(Some(&log), None, |_| HarnessOutcome::Skipped);
        let recorded_early = log.exists();
        record_in(Some(&log), Some(&harness), |_| HarnessOutcome::Skipped);

        let written = fs::read_to_string(&log);
        fs::remove_file(&log).unwrap();
        assert!(!recorded_early);
        assert_eq!(serde_json::from_str::<HarnessSummary>(written.unwrap().trim_end()).unwrap().outcome, HarnessOutcome::Skipped);
    }

    #[test]
    fn a_log_in_a_missing_directory_gets_no_summary() {
        let log = env::current_exe().unwrap().with_file_name(format!("mutest-no-such-dir-{}", process::id())).join("log");

        record_in(Some(&log), Some(&Harness::new(None, None, "krate", None)), |_| HarnessOutcome::Skipped);

        assert!(!log.exists());
    }
}
