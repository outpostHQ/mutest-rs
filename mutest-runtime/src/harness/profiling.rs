//! The reference test run, which a worker that takes over from a crashed one does not run again.

use mutest_exit_code as exit_code;

use super::{ProfiledTest, profile_tests, test};
use crate::journal::WorkerJournal;
use crate::test_runner::{self, TestResult};

/// Profiles the tests without mutations, or takes the profile of the crashed worker before this one.
pub(super) fn reference_run(tests: Vec<test::TestDescAndFn>, journal: Option<&WorkerJournal>) -> Vec<ProfiledTest> {
    match journal.and_then(|journal| journal.earlier_profile(tests.iter().map(|test| test.desc.name.as_slice()))) {
        Some(profile) => {
            println!("reusing the reference test run of the crashed worker");
            tests.into_iter().zip(profile).map(|(test, (exec_time, ignored))| ProfiledTest { test, result: result_of(ignored), exec_time }).collect()
        }
        None => profile_passing_tests(tests, journal),
    }
}

fn profile_passing_tests(tests: Vec<test::TestDescAndFn>, journal: Option<&WorkerJournal>) -> Vec<ProfiledTest> {
    println!("profiling reference test run");
    let Ok(profiled_tests) = profile_tests(tests);
    exit_unless_all_pass(&profiled_tests);
    journal.inspect(|journal| journal.profiled(profiled_tests.iter().map(|test| (test.test.desc.name.as_slice(), test.exec_time, matches!(test.result, TestResult::Ignored)))));
    profiled_tests
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
