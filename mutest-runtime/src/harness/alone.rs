//! The tests of a mutation that runs alone, while no other mutant is active.

use super::{MutationAnalysis, clone_tests, is_reachable_test, order_tests, print_mutation, timeouts};
use crate::metadata::{MutationMeta, SubstMap};
use crate::test_runner;

/// Prints `mutation`, and gives the tests that reach it in the order they run; a timeout rerun gives them longer limits.
pub(super) fn tests<S: SubstMap>(analysis: &MutationAnalysis<'_, S>, mutation: &'static MutationMeta, timeout_rerun: bool) -> Vec<test_runner::Test> {
    let &MutationAnalysis { opts, tests, external_tests_extra, .. } = analysis;

    println!("{}", if timeout_rerun { "confirming timeout of mutation alone:" } else { "applying mutation:" });
    print_mutation(mutation, opts.verbosity);
    println!();

    // The other mutations of a batch stay active, but no test of this mutation reaches them.
    let mut tests = clone_tests(tests.iter().filter(|test| is_reachable_test(mutation, &test.desc, external_tests_extra)));
    if timeout_rerun {
        tests.iter_mut().for_each(|test| test.timeout = test.timeout.map(timeouts::confirmation_timeout));
    }
    order_tests(opts, &mut tests, external_tests_extra, &[mutation]);
    tests
}
