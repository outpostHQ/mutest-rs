//@ print-mutations
//@ run: exit 3
//@ stdout
//@ stderr: empty
//@ mutation-operators: bool_expr_negate
//@ run-flags: -v --exhaustive

//! A function with no active mutation runs its original code, which has no location to cancel a timed out test at.
//! Each loop of that code must cancel the test, or its thread would run on beside the tests of later mutations.

use std::sync::atomic::{self, AtomicU64};
use std::thread;
use std::time::Duration;

static TURNS: AtomicU64 = AtomicU64::new(0);

fn turns() -> u64 {
    // NOTE: The first mutation makes the loop of `work` endless.
    if true { 3 } else { u64::MAX }
}

fn work() {
    for _ in 0..turns() {
        #[mutest::ignore]
        TURNS.fetch_add(1, atomic::Ordering::SeqCst);
    }

    // NOTE: The second mutation gives `work` substitutions of its own, so that the first mutation
    //       runs its original code, and asserts that the test of the first mutation has stopped.
    if false {
        assert_first_mutation_cancelled()
    }
}

#[mutest::skip]
fn assert_first_mutation_cancelled() {
    let turns = TURNS.load(atomic::Ordering::SeqCst);
    thread::sleep(Duration::from_millis(100));
    assert_eq!(turns, TURNS.load(atomic::Ordering::SeqCst));
}

#[test]
fn test() {
    work();
}
