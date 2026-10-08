//@ print-mutations
//@ run: exit 2
//@ stdout
//@ stderr: empty
//@ mutation-operators: fn_return_default
//@ run-flags: --isolate=all

//! Isolated tests that run in turn in one child process each run on their own thread and catch their own panics,
//! as tests in process do, so neither a caught panic nor a thread-local of an earlier test detects a mutation.

use std::cell::Cell;

thread_local! {
    static SET: Cell<bool> = const { Cell::new(false) };
}

fn record(flag: bool) -> bool {
    flag
}

#[test]
fn catch_a_panic() {
    let _ = record(true);
    assert!(std::panic::catch_unwind(|| panic!("caught")).is_err());
}

#[test]
fn first_set_a_thread_local() {
    let _ = record(true);
    SET.set(true);
}

#[test]
fn then_find_the_thread_local_unset() {
    let _ = record(true);
    assert!(!SET.get());
}
