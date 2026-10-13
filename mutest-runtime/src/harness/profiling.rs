//! The reference test run, which a worker that takes over from a crashed one does not run again.

use std::convert::Infallible;
use std::time::Duration;

use mutest_exit_code as exit_code;

use super::{ProfiledTest, runnable_tests, test, unrepeatable};
use crate::config::MutationIsolation;
use crate::journal::WorkerJournal;
use crate::test_runner::{self, TestResult};

/// Profiles the tests without mutations, or takes the profile of the crashed worker before this one.
pub(super) fn reference_run(tests: Vec<test::TestDescAndFn>, journal: Option<&WorkerJournal>, isolation: MutationIsolation) -> Vec<ProfiledTest> {
    match journal.and_then(|journal| journal.earlier_profile(tests.iter().map(|test| test.desc.name.as_slice()))) {
        Some(profile) => {
            println!("reusing the reference test run of the crashed worker");
            tests.into_iter().zip(profile).map(|(test, (exec_time, ignored, unrepeatable))| ProfiledTest { test, result: result_of(ignored), exec_time, unrepeatable, failure_output: Vec::new() }).collect()
        }
        None => profile_passing_tests(tests, journal, isolation),
    }
}

fn profile_passing_tests(tests: Vec<test::TestDescAndFn>, journal: Option<&WorkerJournal>, isolation: MutationIsolation) -> Vec<ProfiledTest> {
    println!("profiling reference test run");
    let Ok(mut profiled_tests) = profile_tests(tests);
    exit_unless_all_pass(&profiled_tests);
    // With every test in a child process, no test runs twice in one process.
    if !matches!(isolation, MutationIsolation::All) { unrepeatable::find(&mut profiled_tests); }
    journal.inspect(|journal| journal.profiled(profiled_tests.iter().map(|test| (test.test.desc.name.as_slice(), test.exec_time, matches!(test.result, TestResult::Ignored), test.unrepeatable))));
    profiled_tests
}

fn profile_tests(tests: Vec<test::TestDescAndFn>) -> Result<Vec<ProfiledTest>, Infallible> {
    let tests_to_run = runnable_tests(&tests);

    let mut profiled_tests = Vec::<ProfiledTest>::with_capacity(tests.len());
    let mut remaining_tests = tests;

    let on_test_event = |event, _remaining_tests: &mut Vec<(test::TestId, test_runner::Test)>| -> Result<_, Infallible> {
        match event {
            test_runner::TestEvent::Result(test) => {
                let test_desc_and_fn = remaining_tests
                    .extract_if(.., |t| t.desc.name == test.desc.name)
                    .next().expect("completed test not found among remaining tests");

                let failed = !matches!(test.result, TestResult::Ok | TestResult::Ignored);
                profiled_tests.push(ProfiledTest {
                    test: test_desc_and_fn,
                    result: test.result,
                    exec_time: test.exec_time,
                    unrepeatable: false,
                    failure_output: if failed { test.stdout } else { Vec::new() },
                });
            }
            _ => {}
        }

        Ok(test_runner::Flow::Continue)
    };

    test_runner::run_tests_with_progress(tests_to_run, on_test_event, test_runner::TestRunStrategy::InProcess(None), false,
        test_runner::progress::Context { phase: "reference", mutation_ids: &|_| Vec::new() }, None)?;

    Ok(profiled_tests)
}

/// The limit of a test: five times its time in the reference run, but at least one second more than that time.
/// A loaded machine and the code of an active mutant both slow a test that still ends, and neither slows it this much.
pub(super) fn auto_test_timeout(exec_time: Duration) -> Duration {
    Ord::max(exec_time * 5, exec_time + Duration::from_secs(1))
}

fn exit_unless_all_pass(profiled_tests: &[ProfiledTest]) {
    let failed_profiled_tests = profiled_tests.iter().filter(|test| !matches!(test.result, TestResult::Ignored | TestResult::Ok)).collect::<Vec<_>>();
    if !failed_profiled_tests.is_empty() {
        for failed_profiled_test in &failed_profiled_tests {
            let name = failed_profiled_test.test.desc.name.as_slice();
            print!("{}", failure_report(name, &failed_profiled_test.result, &failed_profiled_test.failure_output));
            println!("  test {name} ... fail");
        }
        println!("not all tests passed, cannot continue");
        crate::summary::record_reference_failure(failed_profiled_tests.iter().map(|test| test.test.desc.name.as_slice().to_owned()).collect());
        test_runner::progress::exit_incomplete(exit_code::BASELINE_FAILED);
    }
}

/// The output of a failed test as libtest shows it: what the test printed, then the note of the runner.
fn failure_report(name: &str, result: &TestResult, output: &[u8]) -> String {
    let mut output = String::from_utf8_lossy(output).into_owned();
    if let TestResult::FailedMsg(msg) | TestResult::CrashedMsg(msg) = result {
        output.push_str(&format!("note: {msg}"));
    }
    if output.is_empty() { return String::new(); }
    format!("---- {name} stdout ----\n{output}\n")
}

fn result_of(ignored: bool) -> TestResult {
    if ignored { TestResult::Ignored } else { TestResult::Ok }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_report_shows_the_output_and_the_note() {
        assert_eq!(failure_report("a", &TestResult::Failed, b"panicked\n"), "---- a stdout ----\npanicked\n\n");
        assert_eq!(failure_report("a", &TestResult::FailedMsg(String::from("no panic")), b"out\n"), "---- a stdout ----\nout\nnote: no panic\n");
        assert_eq!(failure_report("a", &TestResult::Failed, b""), "");
    }
}
