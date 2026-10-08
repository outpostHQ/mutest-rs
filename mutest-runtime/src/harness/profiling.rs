//! The reference test run, which a worker that takes over from a crashed one does not run again.

use std::time::Duration;

use mutest_exit_code as exit_code;

use super::{ProfiledTest, profile_tests, test, unrepeatable};
use crate::config::MutationIsolation;
use crate::journal::WorkerJournal;
use crate::test_runner::{self, TestResult};

/// Profiles the tests without mutations, or takes the profile of the crashed worker before this one.
pub(super) fn reference_run(tests: Vec<test::TestDescAndFn>, journal: Option<&WorkerJournal>, isolation: MutationIsolation) -> Vec<ProfiledTest> {
    match journal.and_then(|journal| journal.earlier_profile(tests.iter().map(|test| test.desc.name.as_slice()))) {
        Some(profile) => {
            println!("reusing the reference test run of the crashed worker");
            tests.into_iter().zip(profile).map(|(test, (exec_time, ignored, unrepeatable))| ProfiledTest { test, result: result_of(ignored), exec_time, unrepeatable }).collect()
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

/// The limit of a test: its time in the reference run, and half more, but at least one second more,
/// as code with an active mutant runs slower than in the reference run.
pub(super) fn auto_test_timeout(exec_time: Duration) -> Duration {
    exec_time + Ord::max(exec_time.mul_f32(0.5), Duration::from_secs(1))
}

fn exit_unless_all_pass(profiled_tests: &[ProfiledTest]) {
    let failed_profiled_tests = profiled_tests.iter().filter(|test| !matches!(test.result, TestResult::Ignored | TestResult::Ok)).collect::<Vec<_>>();
    if !failed_profiled_tests.is_empty() {
        for failed_profiled_test in failed_profiled_tests {
            println!("  test {} ... fail", failed_profiled_test.test.desc.name.as_slice());
        }
        println!("not all tests passed, cannot continue");
        test_runner::progress::exit_incomplete(exit_code::BASELINE_FAILED);
    }
}

fn result_of(ignored: bool) -> TestResult {
    if ignored { TestResult::Ignored } else { TestResult::Ok }
}
