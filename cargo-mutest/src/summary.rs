//! The `summary.json` of a run, in the format of `mutest_json::summary`, from the lines its test harnesses log.
//! cargo-mutest does not link mutest-json: a build together with mutest-driver would turn on its `rustc` feature.

use std::fs;
use std::io;
use std::path::Path;

use serde_json::Value;

/// See `mutest_json::summary::LOG_VAR`.
pub const LOG_VAR: &str = "MUTEST_SUMMARY_LOG";

/// See `mutest_json::FORMAT_VERSION`.
const FORMAT_VERSION: u32 = 1;

/// The summary of a run, which leaves out each line of the log that does not parse, such as one cut short by a crash.
pub fn run_summary(exit_code: i32, failed_builds: &[&str], summary_log: &str) -> Value {
    let mut harnesses = summary_log.lines().filter_map(|line| serde_json::from_str::<Value>(line).ok()).collect::<Vec<_>>();
    // NOTE: The sort is stable, so the evaluations of a `--flakes` run stay in order.
    harnesses.sort_by_cached_key(|harness| ["package", "target_kind", "target"].map(|key| harness[key].to_string()));
    serde_json::json!({
        "format_version": FORMAT_VERSION,
        "exit_code": exit_code,
        "failed_builds": failed_builds,
        "harnesses": harnesses,
    })
}

/// Writes the summary of a run to `path`, if one is given, or warns that it cannot.
pub fn write_run(path: Option<&Path>, exit_code: i32, failed_builds: &[&str], summary_log: &str) {
    let Some(path) = path else { return };
    if let Err(error) = write(path, &run_summary(exit_code, failed_builds, summary_log)) {
        color_print::ceprintln!("<yellow,bold>warning</>: cannot write the summary of the run to `{}`: {error}", path.display());
    }
}

fn write(path: &Path, summary: &Value) -> io::Result<()> {
    if let Some(dir) = path.parent() { fs::create_dir_all(dir)?; }
    fs::write(path, serde_json::to_vec_pretty(summary)?)
}

#[cfg(test)]
mod tests {
    use std::{env, fs, process};

    use mutest_json::summary::{HarnessOutcome, RunSummary};

    use super::{LOG_VAR, run_summary, write_run};

    #[test]
    fn the_summary_holds_each_whole_line_by_target_in_the_format_of_mutest_json() {
        let summary_log = concat!(
            r#"{"package":"pkg","target_kind":"test","target":"cli","metadata_dir":"pkg/tests/cli","outcome":{"result":"skipped"}}"#, "\n",
            r#"{"package":"pkg","target_kind":"lib","target":"pkg","metadata_dir":"pkg/lib","outcome":{"result":"reference_failed","failed_tests":["a"]}}"#, "\n",
            r#"{"package":"pkg","target_kind":"bin","target":"#, "\n",
        );

        let summary = serde_json::from_value::<RunSummary>(run_summary(4, &["the test harness of `tool` in package `pkg`"], summary_log)).unwrap();

        assert_eq!(LOG_VAR, mutest_json::summary::LOG_VAR);
        assert_eq!(summary.format_version, mutest_json::FORMAT_VERSION);
        assert_eq!(summary.exit_code, 4);
        assert_eq!(summary.failed_builds, ["the test harness of `tool` in package `pkg`"]);
        assert_eq!(summary.harnesses.iter().map(|harness| harness.target.as_str()).collect::<Vec<_>>(), ["pkg", "cli"]);
        assert_eq!(summary.harnesses[0].outcome, HarnessOutcome::ReferenceFailed { failed_tests: vec!["a".to_owned()] });
        assert_eq!(summary.harnesses[1].outcome, HarnessOutcome::Skipped);
    }

    #[test]
    fn a_run_writes_its_summary_where_it_is_asked_to_and_nowhere_else() {
        let dir = env::current_exe().unwrap().with_file_name(format!("mutest-run-summary-{}", process::id()));
        let path = dir.join("json").join("summary.json");
        let _ = fs::remove_dir_all(&dir);

        write_run(None, 0, &[], "");
        assert!(!dir.exists());

        write_run(Some(&path), 2, &[], "");
        let written = fs::read_to_string(&path);
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(serde_json::from_str::<RunSummary>(&written.unwrap()).unwrap().exit_code, 2);
    }

    #[test]
    fn a_summary_that_cannot_be_written_leaves_no_file() {
        let blocker = env::current_exe().unwrap().with_file_name(format!("mutest-run-summary-blocker-{}", process::id()));
        fs::write(&blocker, "").unwrap();
        let path = blocker.join("summary.json");

        write_run(Some(&path), 1, &[], "");
        let written = path.exists();
        fs::remove_file(&blocker).unwrap();
        assert!(!written);
    }
}
