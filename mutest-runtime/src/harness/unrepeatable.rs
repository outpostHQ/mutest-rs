//! A test that keeps state in its process, such as a `OnceLock` it sets, can fail when it runs there again.
//! The reference run finds these tests, and each mutation runs them in child processes, after its other tests.

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::sync::Arc;

use super::{MUTEST_ISOLATED_WORKER_MUTATION_ID, MutationTestResult, MutationTestResults, ProfiledTest, is_reachable_test, make_owned_test_fn, profiling, test};
#[cfg(all(test, target_os = "linux"))]
use super::regression_fixture;
use crate::metadata::{ExternalTestsExtra, MutationMeta};
use crate::test_runner::{self, TestResult, TestRunStrategy};

/// Runs each passing test once more in this process, and marks each test that does not pass again.
pub(super) fn find(profiled_tests: &mut [ProfiledTest]) {
    let tests = profiled_tests.iter()
        .filter(|profiled_test| matches!(profiled_test.result, TestResult::Ok))
        .map(|profiled_test| test_runner::Test {
            desc: profiled_test.test.desc.clone(),
            test_fn: make_owned_test_fn(&profiled_test.test.testfn),
            timeout: profiled_test.exec_time.map(profiling::auto_test_timeout),
            unrepeatable: false,
        })
        .collect::<Vec<_>>();

    let mut failed = HashSet::<test::TestName>::new();
    let Ok(_) = test_runner::run_tests_with_progress(tests, |event, _| {
        if let test_runner::TestEvent::Result(test) = event && !matches!(test.result, TestResult::Ok) {
            failed.insert(test.desc.name);
        }
        Ok::<_, Infallible>(test_runner::Flow::Continue)
    }, TestRunStrategy::InProcess(None), false, test_runner::progress::Context { phase: "reference", mutation_ids: &|_| Vec::new() }, None);

    let mut marked = profiled_tests.iter_mut().filter(|profiled_test| failed.contains(&profiled_test.test.desc.name)).peekable();
    if marked.peek().is_some() { println!("tests that fail when they run again in one process, which each mutation runs in child processes:"); }
    for profiled_test in marked {
        println!("  test {}", profiled_test.test.desc.name.as_slice());
        profiled_test.unrepeatable = true;
    }
}

/// The tests that run in this process, and the tests that run in child processes.
pub(super) fn split(tests: Vec<test_runner::Test>, isolated: bool) -> (Vec<test_runner::Test>, Vec<test_runner::Test>) {
    tests.into_iter().partition(|test| !isolated && !test.unrepeatable)
}

/// The tests still to run after the tests of this process, which stop at the first detection unless `exhaustive`.
pub(super) fn not_yet_detected(tests: Vec<test_runner::Test>, results: &HashMap<u32, MutationTestResults>, exhaustive: bool, mutations: &[&'static MutationMeta], external_tests_extra: Option<&ExternalTestsExtra>) -> Vec<test_runner::Test> {
    let undetected = |mutation: &&&'static MutationMeta| matches!(results[&mutation.id].result, MutationTestResult::Undetected);
    tests.into_iter()
        .filter(|test| exhaustive || mutations.iter().filter(undetected).any(|mutation| is_reachable_test(mutation, &test.desc, external_tests_extra)))
        .collect()
}

/// Runs each test in a child process, which activates the substitutions of the whole mutant that holds the mutation.
pub(super) fn in_child_processes(mutation_id: u32) -> TestRunStrategy {
    TestRunStrategy::InIsolatedChildProcess(Arc::new(move |cmd| {
        cmd.env(MUTEST_ISOLATED_WORKER_MUTATION_ID, mutation_id.to_string());
        #[cfg(all(test, target_os = "linux"))]
        regression_fixture::run_fixture_entry_only(cmd);
    }))
}
